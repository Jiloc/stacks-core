// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2025 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command as StdCommand;

use anyhow::{Context, Result};
use clap::{Arg, Command as ClapCommand};
use once_cell::sync::Lazy;
use serde::Serialize;

// Static regex for finding constant references in documentation
static CONSTANT_REFERENCE_REGEX: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\[`([A-Z_][A-Z0-9_]*)`\]").unwrap());

#[derive(Debug, Serialize)]
pub struct FieldDoc {
    pub name: String,
    pub description: String,
    pub default_value: Option<String>,
    pub notes: Option<Vec<String>>,
    pub deprecated: Option<String>,
    pub toml_example: Option<String>,
    pub required: Option<bool>,
    pub units: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StructDoc {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<FieldDoc>,
}

#[derive(Debug, Serialize)]
struct ConfigDocs {
    structs: Vec<StructDoc>,
    referenced_constants: HashMap<String, Option<String>>, // Name -> Resolved Value (or None)
    // Add mapping from TOML section names to struct names
    section_to_struct_mapping: HashMap<String, String>, // section_name -> struct_name
}

// JSON navigation helper functions
/// Navigate through nested JSON structure using an array of keys
/// Returns None if any part of the path doesn't exist
///
/// Example: get_json_path(value, &["inner", "struct", "kind"])
/// is equivalent to value.get("inner")?.get("struct")?.get("kind")
fn get_json_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut current = value;

    for &key in path {
        current = current.get(key)?;
    }

    Some(current)
}

/// Navigate to an array at the given JSON path
/// Returns None if the path doesn't exist or the value is not an array
fn get_json_array<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a Vec<serde_json::Value>> {
    get_json_path(value, path)?.as_array()
}

/// Navigate to an object at the given JSON path
/// Returns None if the path doesn't exist or the value is not an object
fn get_json_object<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    get_json_path(value, path)?.as_object()
}

/// Navigate to a string at the given JSON path
/// Returns None if the path doesn't exist or the value is not a string
fn get_json_string<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    get_json_path(value, path)?.as_str()
}

fn main() -> Result<()> {
    let matches = ClapCommand::new("extract-docs")
        .about("Extract documentation from Rust source code using rustdoc JSON")
        .arg(
            Arg::new("package")
                .long("package")
                .short('p')
                .value_name("PACKAGE")
                .help("Package to extract docs for")
                .required(true),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .value_name("FILE")
                .help("Output JSON file")
                .required(true),
        )
        .arg(
            Arg::new("main-config-struct")
                .long("main-config-struct")
                .value_name("STRUCT_NAME")
                .help("Main configuration struct name (e.g., 'ConfigFile')")
                .required(true),
        )
        .get_matches();

    let package = matches.get_one::<String>("package").unwrap();
    let output_file = matches.get_one::<String>("output").unwrap();
    let main_struct_name = matches.get_one::<String>("main-config-struct").unwrap();

    // Generate rustdoc JSON
    let rustdoc_json = generate_rustdoc_json(package)?;

    // Automatically derive config structs from the main config struct
    let (target_structs, section_to_struct_mapping) =
        derive_config_structs_from_main(&rustdoc_json, main_struct_name)?;
    println!("Auto-discovered config structs: {:?}", target_structs);

    // Extract configuration documentation from the rustdoc JSON
    let config_docs = extract_config_docs_from_rustdoc(
        &rustdoc_json,
        &Some(target_structs),
        section_to_struct_mapping,
    )?;

    // Write the extracted docs to file
    fs::write(output_file, serde_json::to_string_pretty(&config_docs)?)?;

    println!("Successfully extracted documentation to {}", output_file);
    println!(
        "Found {} structs with documentation",
        config_docs.structs.len()
    );
    Ok(())
}

/// Extract the target struct name and collection information from a field's type information
/// Returns (struct_name, is_collection) where is_collection indicates if it's Vec<T> or similar
fn extract_field_type_info(
    field_item: &serde_json::Value,
    index_obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, bool)> {
    // Navigate to the field's type information
    // Path varies depending on rustdoc structure, try different possible paths

    // Try: field.inner.struct_field.resolved_path (for newer rustdoc versions)
    if let Some(type_info) = get_json_path(field_item, &["inner", "struct_field", "resolved_path"])
    {
        return parse_type_for_struct_name(type_info, index_obj);
    }

    // Try: field.inner.struct_field.type (for struct fields)
    if let Some(type_info) = get_json_path(field_item, &["inner", "struct_field", "type"]) {
        return parse_type_for_struct_name(type_info, index_obj);
    }

    // Try: field.inner.type (alternative structure)
    if let Some(type_info) = get_json_path(field_item, &["inner", "type"]) {
        return parse_type_for_struct_name(type_info, index_obj);
    }

    // Try: field.type (direct type field)
    if let Some(type_info) = get_json_path(field_item, &["type"]) {
        return parse_type_for_struct_name(type_info, index_obj);
    }

    None
}

/// Parse type information to extract struct name and determine if it's a collection
/// Only supports Vec<T> and HashSet<T> containers, returns (struct_name, is_collection)
fn parse_type_for_struct_name(
    type_info: &serde_json::Value,
    index_obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, bool)> {
    // Handle direct resolved_path case (for rustdoc's new structure)
    if let Some(path) = get_json_string(type_info, &["path"]) {
        // Check if this is a supported container type
        if path == "Vec" || path.ends_with("HashSet") {
            // Look for the first type argument to find the inner type
            if let Some(args_array) =
                get_json_array(type_info, &["args", "angle_bracketed", "args"])
            {
                if let Some(first_arg) = args_array.first() {
                    if let Some(inner_type) = get_json_path(first_arg, &["type"]) {
                        if let Some((inner_struct, _)) =
                            parse_type_for_struct_name(inner_type, index_obj)
                        {
                            return Some((inner_struct, true)); // Vec and HashSet are collections
                        }
                    }
                }
            }
        } else if path == "Option" {
            // Handle Option<T> wrapper - inherit collection status from inner type
            if let Some(args_array) =
                get_json_array(type_info, &["args", "angle_bracketed", "args"])
            {
                if let Some(first_arg) = args_array.first() {
                    if let Some(inner_type) = get_json_path(first_arg, &["type"]) {
                        if let Some((inner_struct, is_collection)) =
                            parse_type_for_struct_name(inner_type, index_obj)
                        {
                            return Some((inner_struct, is_collection)); // Inherit collection status
                        }
                    }
                }
            }
        } else {
            // Not a container, check if it's a struct that exists in the index
            if struct_exists_in_index(path, index_obj) {
                return Some((path.to_string(), false));
            }
        }
    }

    // Handle resolved_path object structure
    if let Some(resolved_path) = get_json_object(type_info, &["resolved_path"]) {
        // Check the 'path' field first
        if let Some(path) =
            get_json_string(&serde_json::Value::Object(resolved_path.clone()), &["path"])
        {
            // Check if this is a supported container type
            if path == "Vec" || path.ends_with("HashSet") {
                // Look for type arguments in the resolved_path structure
                if let Some(args_array) = get_json_array(
                    &serde_json::Value::Object(resolved_path.clone()),
                    &["args", "angle_bracketed", "args"],
                ) {
                    if let Some(first_arg) = args_array.first() {
                        if let Some(inner_type) = get_json_path(first_arg, &["type"]) {
                            if let Some((inner_struct, _)) =
                                parse_type_for_struct_name(inner_type, index_obj)
                            {
                                return Some((inner_struct, true)); // Vec and HashSet are collections
                            }
                        }
                    }
                }
            } else {
                // Not a container, check if it's a struct that exists in the index
                if struct_exists_in_index(path, index_obj) {
                    return Some((path.to_string(), false));
                }
            }
        }

        // Check the 'name' field as fallback
        if let Some(name) =
            get_json_string(&serde_json::Value::Object(resolved_path.clone()), &["name"])
        {
            if struct_exists_in_index(name, index_obj) {
                return Some((name.to_string(), false));
            }
        }
    }

    None
}

/// Check if a struct exists in the rustdoc index
fn struct_exists_in_index(
    name: &str,
    index_obj: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    index_obj.values().any(|item| {
        get_json_string(item, &["name"]) == Some(name)
            && get_json_object(item, &["inner", "struct"]).is_some()
    })
}

/// Automatically derive configuration struct names and TOML section mappings from the main config struct
/// by examining its fields and finding those that don't have @ignore annotation
fn derive_config_structs_from_main(
    rustdoc_json: &serde_json::Value,
    main_struct_name: &str,
) -> Result<(Vec<String>, HashMap<String, String>)> {
    let index = get_json_path(rustdoc_json, &["index"])
        .ok_or_else(|| anyhow::anyhow!("Missing 'index' in rustdoc JSON"))?;

    let index_obj = index
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Index is not an object"))?;

    // Find the main config struct in the rustdoc index
    let main_struct_item = index_obj
        .values()
        .find(|item| {
            get_json_string(item, &["name"]) == Some(main_struct_name)
                && get_json_object(item, &["inner", "struct"]).is_some()
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Main config struct '{}' not found in rustdoc JSON",
                main_struct_name
            )
        })?;

    let mut config_structs = Vec::new();
    let mut section_to_struct_mapping = HashMap::new();

    // Extract fields from the main config struct
    if let Some(field_ids) = get_json_array(
        main_struct_item,
        &["inner", "struct", "kind", "plain", "fields"],
    ) {
        for field_id in field_ids {
            // Field IDs can be either integers or strings in rustdoc JSON
            let field_item = if let Some(field_id_num) = field_id.as_u64() {
                index_obj.get(&field_id_num.to_string())
            } else if let Some(field_id_str) = field_id.as_str() {
                index_obj.get(field_id_str)
            } else {
                continue;
            };

            if let Some(field_item) = field_item {
                // Extract field name
                let field_name = match get_json_string(field_item, &["name"]) {
                    Some(name) => name,
                    None => continue,
                };

                // Check if field should be ignored
                if let Some(field_docs) = get_json_string(field_item, &["docs"]) {
                    if field_docs.contains("@ignore") {
                        println!("Ignoring field '{}' due to @ignore annotation", field_name);
                        continue;
                    }
                }

                // Extract the target struct name from the field type information
                if let Some((struct_name, is_collection)) =
                    extract_field_type_info(field_item, index_obj)
                {
                    // Generate TOML section name from field name
                    let section_name = if is_collection {
                        format!("[[{}]]", field_name)
                    } else {
                        format!("[{}]", field_name)
                    };

                    // Verify the struct exists in the rustdoc index
                    let struct_exists = index_obj.values().any(|item| {
                        get_json_string(item, &["name"]) == Some(&struct_name)
                            && get_json_object(item, &["inner", "struct"]).is_some()
                    });

                    if struct_exists {
                        config_structs.push(struct_name.clone());
                        section_to_struct_mapping.insert(section_name.clone(), struct_name.clone());
                        println!(
                            "Mapped field '{}' -> struct '{}' -> section '{}'",
                            field_name, struct_name, section_name
                        );
                    } else {
                        println!(
                            "Warning: Struct '{}' for field '{}' not found in rustdoc",
                            struct_name, field_name
                        );
                    }
                } else {
                    println!(
                        "Warning: Could not extract type information for field '{}', skipping",
                        field_name
                    );
                }
            }
        }
    }

    // Remove duplicates from config_structs
    config_structs.sort();
    config_structs.dedup();

    println!("Final discovered structs: {:?}", config_structs);
    println!("Final section mappings: {:?}", section_to_struct_mapping);

    Ok((config_structs, section_to_struct_mapping))
}

fn generate_rustdoc_json(package: &str) -> Result<serde_json::Value> {
    // List of crates to generate rustdoc for (in addition to the main package)
    // These crates contain constants that might be referenced in documentation
    // NOTE: This list must be manually updated if new dependencies containing
    // constants referenced in doc comments are added to the project
    let additional_crates = ["stacks-common"];

    // Respect CARGO_TARGET_DIR environment variable for rustdoc output
    let rustdoc_target_dir = std::env::var("CARGO_TARGET_DIR")
        .unwrap_or_else(|_| "target".to_string())
        + "/rustdoc-json";

    // WARNING: This tool relies on nightly rustdoc JSON output (-Z unstable-options --output-format json)
    // The JSON format is subject to change with new Rust nightly versions and could break this tool.
    // Use cargo rustdoc with nightly to generate JSON for the main package
    let output = StdCommand::new("cargo")
        .args([
            "+nightly",
            "rustdoc",
            "--lib",
            "-p",
            package,
            "--target-dir",
            &rustdoc_target_dir,
            "--",
            "-Z",
            "unstable-options",
            "--output-format",
            "json",
            "--document-private-items",
        ])
        .output()
        .context("Failed to run cargo rustdoc command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cargo rustdoc failed: {}", stderr);
    }

    // Generate rustdoc for additional crates that might contain referenced constants
    for additional_crate in &additional_crates {
        let error_msg = format!(
            "Failed to run cargo rustdoc command for {}",
            additional_crate
        );
        let output = StdCommand::new("cargo")
            .args([
                "+nightly",
                "rustdoc",
                "--lib",
                "-p",
                additional_crate,
                "--target-dir",
                &rustdoc_target_dir,
                "--",
                "-Z",
                "unstable-options",
                "--output-format",
                "json",
                "--document-private-items",
            ])
            .output()
            .context(error_msg)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "Warning: Failed to generate rustdoc for {}: {}",
                additional_crate, stderr
            );
        }
    }

    // Map package names to their library names if different
    // For most packages, the library name is the same as package name with hyphens replaced by underscores
    // But some packages have custom library names defined in Cargo.toml
    // NOTE: This mapping must be updated if new packages with different library names are processed
    let lib_name = match package {
        "stackslib" => "blockstack_lib".to_string(),
        _ => package.replace('-', "_"),
    };

    // Read the generated JSON file - rustdoc generates it based on library name
    let json_file_path = format!("{}/doc/{}.json", rustdoc_target_dir, lib_name);
    let json_content = std::fs::read_to_string(json_file_path)
        .context("Failed to read generated rustdoc JSON file")?;

    serde_json::from_str(&json_content).context("Failed to parse rustdoc JSON output")
}

fn extract_config_docs_from_rustdoc(
    rustdoc_json: &serde_json::Value,
    target_structs: &Option<Vec<String>>,
    section_to_struct_mapping: HashMap<String, String>,
) -> Result<ConfigDocs> {
    let mut structs = Vec::new();
    let mut all_referenced_constants = std::collections::HashSet::new();

    // Access the main index containing all items from the rustdoc JSON output
    let index = get_json_object(rustdoc_json, &["index"])
        .context("Missing 'index' field in rustdoc JSON")?;

    for (_item_id, item) in index {
        // Extract the item's name from rustdoc JSON structure
        if let Some(name) = get_json_string(item, &["name"]) {
            // Check if this item is a struct by looking for the "struct" field
            if get_json_object(item, &["inner", "struct"]).is_some() {
                // Check if this struct is in our target list (if specified)
                if let Some(targets) = target_structs {
                    if !targets.contains(&name.to_string()) {
                        continue;
                    }
                }

                let (struct_doc_opt, referenced_constants) =
                    extract_struct_from_rustdoc_index(index, name, item)?;

                if let Some(struct_doc) = struct_doc_opt {
                    structs.push(struct_doc);
                }
                all_referenced_constants.extend(referenced_constants);
            }
        }
    }

    // Resolve all collected constant references
    let mut referenced_constants = HashMap::new();
    for constant_name in all_referenced_constants {
        let resolved_value = resolve_constant_reference(&constant_name, index);
        referenced_constants.insert(constant_name, resolved_value);
    }

    Ok(ConfigDocs {
        structs,
        referenced_constants,
        section_to_struct_mapping,
    })
}

fn extract_struct_from_rustdoc_index(
    index: &serde_json::Map<String, serde_json::Value>,
    struct_name: &str,
    struct_item: &serde_json::Value,
) -> Result<(Option<StructDoc>, HashSet<String>)> {
    let mut all_referenced_constants = std::collections::HashSet::new();

    // Extract struct documentation
    let description = get_json_string(struct_item, &["docs"]).map(|s| s.to_string());

    // Collect constant references from struct description
    if let Some(desc) = &description {
        all_referenced_constants.extend(find_constant_references(desc));
    }

    // Extract fields
    let (fields, referenced_constants) = extract_struct_fields(index, struct_item)?;

    // Extend referenced constants
    all_referenced_constants.extend(referenced_constants);

    if !fields.is_empty() || description.is_some() {
        let struct_doc = StructDoc {
            name: struct_name.to_string(),
            description,
            fields,
        };
        Ok((Some(struct_doc), all_referenced_constants))
    } else {
        Ok((None, all_referenced_constants))
    }
}

fn extract_struct_fields(
    index: &serde_json::Map<String, serde_json::Value>,
    struct_item: &serde_json::Value,
) -> Result<(Vec<FieldDoc>, std::collections::HashSet<String>)> {
    let mut fields = Vec::new();
    let mut all_referenced_constants = std::collections::HashSet::new();

    // Navigate through rustdoc JSON structure to access struct fields
    // Path: item.inner.struct.kind.plain.fields[]
    if let Some(field_ids) =
        get_json_array(struct_item, &["inner", "struct", "kind", "plain", "fields"])
    {
        for field_id in field_ids {
            // Field IDs can be either integers or strings in rustdoc JSON, try both formats
            let field_item = if let Some(field_id_num) = field_id.as_u64() {
                // Numeric field ID - convert to string for index lookup
                index.get(&field_id_num.to_string())
            } else if let Some(field_id_str) = field_id.as_str() {
                // String field ID - use directly for index lookup
                index.get(field_id_str)
            } else {
                None
            };

            if let Some(field_item) = field_item {
                // Extract the field's name from the rustdoc item
                let field_name = get_json_string(field_item, &["name"])
                    .unwrap_or("unknown")
                    .to_string();

                // Extract the field's documentation text from rustdoc
                let field_docs = get_json_string(field_item, &["docs"])
                    .unwrap_or("")
                    .to_string();

                // Parse the structured documentation
                let (field_doc, referenced_constants) =
                    parse_field_documentation(&field_docs, &field_name)?;

                // Only include fields that have documentation
                if !field_doc.description.is_empty() || field_doc.default_value.is_some() {
                    fields.push(field_doc);
                }

                // Extend referenced constants
                all_referenced_constants.extend(referenced_constants);
            }
        }
    }

    Ok((fields, all_referenced_constants))
}

fn parse_field_documentation(
    doc_text: &str,
    field_name: &str,
) -> Result<(FieldDoc, std::collections::HashSet<String>)> {
    let mut default_value = None;
    let mut notes = None;
    let mut deprecated = None;
    let mut toml_example = None;
    let mut required = None;
    let mut units = None;
    let mut referenced_constants = std::collections::HashSet::new();

    // Split on --- separator if present
    let parts: Vec<&str> = doc_text.split("---").collect();

    let description = parts[0].trim().to_string();

    // Collect constant references from description
    referenced_constants.extend(find_constant_references(&description));

    // Parse metadata section if present
    if parts.len() >= 2 {
        let metadata_section = parts[1];

        // Parse @default: annotations
        if let Some(default_match) = extract_annotation(metadata_section, "default") {
            // Collect constant references from default value
            referenced_constants.extend(find_constant_references(&default_match));
            default_value = Some(default_match);
        }

        // Parse @notes: annotations
        if let Some(notes_text) = extract_annotation(metadata_section, "notes") {
            // Collect constant references from notes
            referenced_constants.extend(find_constant_references(&notes_text));

            let mut note_items: Vec<String> = Vec::new();
            let mut current_note = String::new();
            let mut in_note = false;

            for line in notes_text.lines() {
                let trimmed = line.trim();

                // Skip empty lines
                if trimmed.is_empty() {
                    continue;
                }

                // Check if this line starts a new note (bullet point)
                if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                    // If we were building a previous note, save it
                    if in_note && !current_note.trim().is_empty() {
                        note_items.push(current_note.trim().to_string());
                    }

                    // Start a new note (remove the bullet point)
                    current_note = trimmed[2..].trim().to_string();
                    in_note = true;
                } else if in_note {
                    // This is a continuation line for the current note
                    if !current_note.is_empty() {
                        current_note.push(' ');
                    }
                    current_note.push_str(trimmed);
                }
                // If not in_note and doesn't start with bullet, ignore the line
            }

            // Don't forget the last note
            if in_note && !current_note.trim().is_empty() {
                note_items.push(current_note.trim().to_string());
            }

            if !note_items.is_empty() {
                notes = Some(note_items);
            }
        }

        // Parse @deprecated: annotations
        if let Some(deprecated_text) = extract_annotation(metadata_section, "deprecated") {
            // Collect constant references from deprecated text
            referenced_constants.extend(find_constant_references(&deprecated_text));
            deprecated = Some(deprecated_text);
        }

        // Parse @toml_example: annotations
        if let Some(example_text) = extract_annotation(metadata_section, "toml_example") {
            // Note: We typically don't expect constant references in TOML examples,
            // but we'll check anyway for completeness
            referenced_constants.extend(find_constant_references(&example_text));
            toml_example = Some(example_text);
        }

        // Parse @required: annotations
        if let Some(required_text) = extract_annotation(metadata_section, "required") {
            let required_bool = match required_text.trim() {
                "" => false, // Empty string defaults to false
                text => text.parse::<bool>().unwrap_or_else(|_| {
                    eprintln!(
                        "Warning: Invalid @required value '{}' for field '{}', defaulting to false",
                        text, field_name
                    );
                    false
                }),
            };
            required = Some(required_bool);
        }

        // Parse @units: annotations
        if let Some(units_text) = extract_annotation(metadata_section, "units") {
            // Collect constant references from units text
            referenced_constants.extend(find_constant_references(&units_text));
            units = Some(units_text);
        }
    }

    let field_doc = FieldDoc {
        name: field_name.to_string(),
        description,
        default_value,
        notes,
        deprecated,
        toml_example,
        required,
        units,
    };

    Ok((field_doc, referenced_constants))
}

/// Parse a YAML-style literal block scalar (|) from comment lines
/// Preserves newlines and internal indentation relative to the block base indentation
fn parse_literal_block_scalar(lines: &[&str], _base_indent: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }

    // Find the first non-empty content line to determine block indentation
    let content_lines: Vec<&str> = lines
        .iter()
        .skip_while(|line| line.trim().is_empty())
        .copied()
        .collect();

    if content_lines.is_empty() {
        return String::new();
    }

    // Determine block indentation from the first content line
    let block_indent = content_lines[0].len() - content_lines[0].trim_start().len();

    // Process all lines, preserving relative indentation within the block
    let mut result_lines = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            // Preserve empty lines
            result_lines.push(String::new());
        } else {
            let line_indent = line.len() - line.trim_start().len();
            if line_indent >= block_indent {
                // Remove only the common block indentation, preserving relative indentation
                let content = &line[block_indent.min(line.len())..];
                result_lines.push(content.to_string());
            } else {
                // Line is less indented than block base - should not happen in well-formed blocks
                result_lines.push(line.trim_start().to_string());
            }
        }
    }

    // Remove trailing empty lines (clip chomping style)
    while let Some(last) = result_lines.last() {
        if last.is_empty() {
            result_lines.pop();
        } else {
            break;
        }
    }

    result_lines.join("\n")
}

/// Parse a YAML-style folded block scalar (>)
/// Folds lines into paragraphs, preserving more-indented lines as literal blocks
fn parse_folded_block_scalar(lines: &[&str], _base_indent: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }

    // Find the first non-empty content line to determine block indentation
    let content_lines: Vec<&str> = lines
        .iter()
        .skip_while(|line| line.trim().is_empty())
        .copied()
        .collect();

    if content_lines.is_empty() {
        return String::new();
    }

    // Determine block indentation from the first content line
    let block_indent = content_lines[0].len() - content_lines[0].trim_start().len();

    let mut result = String::new();
    let mut current_paragraph = Vec::new();
    let mut in_literal_block = false;

    for line in lines {
        if line.trim().is_empty() {
            if in_literal_block {
                // Empty line in literal block - preserve it
                result.push('\n');
            } else if !current_paragraph.is_empty() {
                // End current paragraph
                result.push_str(&current_paragraph.join(" "));
                result.push_str("\n\n");
                current_paragraph.clear();
            }
            continue;
        }

        let line_indent = line.len() - line.trim_start().len();
        let content = if line_indent >= block_indent {
            &line[block_indent.min(line.len())..]
        } else {
            line.trim_start()
        };

        let relative_indent = line_indent.saturating_sub(block_indent);

        if relative_indent > 0 {
            // More indented line - start or continue literal block
            if !in_literal_block {
                // Finish current paragraph before starting literal block
                if !current_paragraph.is_empty() {
                    result.push_str(&current_paragraph.join(" "));
                    result.push('\n');
                    current_paragraph.clear();
                }
                in_literal_block = true;
            }
            // Add literal line with preserved indentation
            result.push_str(content);
            result.push('\n');
        } else {
            // Normal indentation - folded content
            if in_literal_block {
                // Exit literal block
                in_literal_block = false;
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
            }
            // Add to current paragraph
            current_paragraph.push(content);
        }
    }

    // Finish any remaining paragraph
    if !current_paragraph.is_empty() {
        result.push_str(&current_paragraph.join(" "));
    }

    // Apply "clip" chomping style (consistent with literal parser)
    // Remove trailing empty lines but preserve a single trailing newline if content exists
    let trimmed = result.trim_end_matches('\n');
    if !trimmed.is_empty() && result.ends_with('\n') {
        format!("{}\n", trimmed)
    } else {
        trimmed.to_string()
    }
}

fn extract_annotation(metadata_section: &str, annotation_name: &str) -> Option<String> {
    let annotation_pattern = format!("@{}:", annotation_name);

    if let Some(_start_pos) = metadata_section.find(&annotation_pattern) {
        // Split the metadata section into lines for processing
        let all_lines: Vec<&str> = metadata_section.lines().collect();

        // Find which line contains our annotation
        let mut annotation_line_idx = None;
        for (idx, line) in all_lines.iter().enumerate() {
            if line.contains(&annotation_pattern) {
                annotation_line_idx = Some(idx);
                break;
            }
        }

        let annotation_line_idx = annotation_line_idx?;
        let annotation_line = all_lines[annotation_line_idx];

        // Find the position of the annotation pattern within this line
        let pattern_pos = annotation_line.find(&annotation_pattern)?;
        let after_colon = &annotation_line[pattern_pos + annotation_pattern.len()..];

        // Check for multiline indicators immediately after the colon
        let trimmed_after_colon = after_colon.trim_start();

        if trimmed_after_colon.starts_with('|') {
            // Literal block scalar mode (|)
            // Content starts from the next line, ignoring any text after | on the same line
            let block_lines = collect_annotation_block_lines(
                &all_lines,
                annotation_line_idx + 1,
                annotation_line,
            );

            // Convert to owned strings for the parser
            let owned_lines: Vec<String> = block_lines.iter().map(|s| s.to_string()).collect();

            // Convert back to string slices for the parser
            let string_refs: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
            let base_indent = annotation_line.len() - annotation_line.trim_start().len();
            let result = parse_literal_block_scalar(&string_refs, base_indent);
            if result.trim().is_empty() {
                return None;
            } else {
                return Some(result);
            }
        } else if trimmed_after_colon.starts_with('>') {
            // Folded block scalar mode (>)
            // Content starts from the next line, ignoring any text after > on the same line
            let block_lines = collect_annotation_block_lines(
                &all_lines,
                annotation_line_idx + 1,
                annotation_line,
            );

            // Convert to owned strings for the parser
            let owned_lines: Vec<String> = block_lines.iter().map(|s| s.to_string()).collect();

            // Convert back to string slices for the parser
            let string_refs: Vec<&str> = owned_lines.iter().map(|s| s.as_str()).collect();
            let base_indent = annotation_line.len() - annotation_line.trim_start().len();
            let result = parse_folded_block_scalar(&string_refs, base_indent);
            if result.trim().is_empty() {
                return None;
            } else {
                return Some(result);
            }
        } else {
            // Default literal-like multiline mode
            // Content can start on the same line or the next line
            let mut content_lines = Vec::new();

            // Check if there's content on the same line after the colon
            if !trimmed_after_colon.is_empty() {
                content_lines.push(trimmed_after_colon);
            }

            // Collect subsequent lines that belong to this annotation
            let block_lines = collect_annotation_block_lines(
                &all_lines,
                annotation_line_idx + 1,
                annotation_line,
            );

            // For default mode, preserve relative indentation within the block
            if !block_lines.is_empty() {
                // Find the base indentation from the first non-empty content line
                let mut base_indent = None;
                for line in &block_lines {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        base_indent = Some(line.len() - line.trim_start().len());
                        break;
                    }
                }

                // Process lines preserving relative indentation
                for line in block_lines {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Some(base) = base_indent {
                            let line_indent = line.len() - line.trim_start().len();
                            if line_indent >= base {
                                // Remove only the common base indentation, preserving relative indentation
                                let content = &line[base.min(line.len())..];
                                content_lines.push(content);
                            } else {
                                // Line is less indented than base - use trimmed content
                                content_lines.push(trimmed);
                            }
                        } else {
                            content_lines.push(trimmed);
                        }
                    }
                }
            }

            if content_lines.is_empty() {
                return None;
            }

            // Join lines preserving the structure - this maintains internal newlines and relative indentation
            let result = content_lines.join("\n");

            // Apply standard trimming and return if not empty
            let cleaned = result.trim();
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }

    None
}

/// Collect lines that belong to an annotation block, stopping at the next annotation or end
fn collect_annotation_block_lines<'a>(
    all_lines: &[&'a str],
    start_idx: usize,
    annotation_line: &str,
) -> Vec<&'a str> {
    let mut block_lines = Vec::new();
    let annotation_indent = annotation_line.len() - annotation_line.trim_start().len();

    for &line in all_lines.iter().skip(start_idx) {
        let trimmed = line.trim();

        // Stop if we hit another annotation at the same or lesser indentation level
        if trimmed.starts_with('@') && trimmed.contains(':') {
            let line_indent = line.len() - line.trim_start().len();
            if line_indent <= annotation_indent {
                break;
            }
        }

        // Stop if we hit a line that's clearly not part of the comment block
        // (very different indentation or structure)
        let line_indent = line.len() - line.trim_start().len();
        if !trimmed.is_empty() && line_indent < annotation_indent {
            break;
        }

        block_lines.push(line);
    }

    block_lines
}

fn resolve_constant_reference(
    name: &str,
    rustdoc_index: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    // First, try to find the constant in the main rustdoc index
    if let Some(value) = resolve_constant_in_index(name, rustdoc_index) {
        return Some(value);
    }

    // If not found in main index, try additional crates
    let additional_crate_libs = ["stacks_common"]; // Library names for additional crates

    for lib_name in &additional_crate_libs {
        let json_file_path = format!("target/rustdoc-json/doc/{}.json", lib_name);
        if let Ok(json_content) = std::fs::read_to_string(&json_file_path) {
            if let Ok(rustdoc_json) = serde_json::from_str::<serde_json::Value>(&json_content) {
                if let Some(index) = get_json_object(&rustdoc_json, &["index"]) {
                    if let Some(value) = resolve_constant_in_index(name, index) {
                        return Some(value);
                    }
                }
            }
        }
    }

    None
}

fn resolve_constant_in_index(
    name: &str,
    rustdoc_index: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    // Look for a constant with the given name in the rustdoc index
    for (_item_id, item) in rustdoc_index {
        // Check if this item's name matches the constant we're looking for
        if let Some(item_name) = get_json_string(item, &["name"]) {
            if item_name == name {
                // Check if this item is a constant by looking for the "constant" field
                if let Some(constant_data) = get_json_object(item, &["inner", "constant"]) {
                    // Try newer rustdoc JSON structure first (with nested 'const' field)
                    let constant_data_value = serde_json::Value::Object(constant_data.clone());
                    if get_json_object(&constant_data_value, &["const"]).is_some() {
                        // For literal constants, prefer expr which doesn't have type suffix
                        if get_json_path(&constant_data_value, &["const", "is_literal"])
                            .and_then(|v| v.as_bool())
                            == Some(true)
                        {
                            // Access the expression field for literal constant values
                            if let Some(expr) =
                                get_json_string(&constant_data_value, &["const", "expr"])
                            {
                                if expr != "_" {
                                    return Some(expr.to_string());
                                }
                            }
                        }

                        // For computed constants or when expr is "_", use value but strip type suffix
                        if let Some(value) =
                            get_json_string(&constant_data_value, &["const", "value"])
                        {
                            return Some(strip_type_suffix(value));
                        }

                        // Fallback to expr if value is not available
                        if let Some(expr) =
                            get_json_string(&constant_data_value, &["const", "expr"])
                        {
                            if expr != "_" {
                                return Some(expr.to_string());
                            }
                        }
                    }

                    // Fall back to older rustdoc JSON structure for compatibility
                    if let Some(value) = get_json_string(&constant_data_value, &["value"]) {
                        return Some(strip_type_suffix(value));
                    }
                    if let Some(expr) = get_json_string(&constant_data_value, &["expr"]) {
                        if expr != "_" {
                            return Some(expr.to_string());
                        }
                    }

                    // For some constants, the value might be in the type field if it's a simple literal
                    if let Some(type_str) = get_json_string(&constant_data_value, &["type"]) {
                        // Handle simple numeric or string literals embedded in type
                        return Some(type_str.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Strip type suffixes from rustdoc constant values (e.g., "50u64" -> "50", "402_653_196u32" -> "402_653_196")
fn strip_type_suffix(value: &str) -> String {
    // Common Rust integer type suffixes
    let suffixes = [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        "f32", "f64",
    ];

    for suffix in &suffixes {
        if let Some(without_suffix) = value.strip_suffix(suffix) {
            // Only strip if the remaining part looks like a numeric literal
            // (contains only digits, underscores, dots, minus signs, or quotes for string literals)
            if !without_suffix.is_empty()
                && (without_suffix
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '_' || c == '.' || c == '-')
                    || (without_suffix.starts_with('"') && without_suffix.ends_with('"')))
            {
                return without_suffix.to_string();
            }
        }
    }

    // If no valid suffix found, return as-is
    value.to_string()
}

fn find_constant_references(text: &str) -> std::collections::HashSet<String> {
    let mut constants = std::collections::HashSet::new();

    for captures in CONSTANT_REFERENCE_REGEX.captures_iter(text) {
        if let Some(constant_name) = captures.get(1) {
            constants.insert(constant_name.as_str().to_string());
        }
    }

    constants
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_parse_field_documentation_basic() {
        let doc_text = "This is a basic field description.";
        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        assert_eq!(result.0.name, "test_field");
        assert_eq!(result.0.description, "This is a basic field description.");
        assert_eq!(result.0.default_value, None);
        assert_eq!(result.0.notes, None);
        assert_eq!(result.0.deprecated, None);
        assert_eq!(result.0.toml_example, None);
    }

    #[test]
    fn test_parse_field_documentation_with_metadata() {
        let doc_text = r#"This is a field with metadata.
---
@default: `"test_value"`
@notes:
  - This is a note.
  - This is another note.
@deprecated: This field is deprecated.
@toml_example: |
  key = "value"
  other = 123"#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        assert_eq!(result.0.name, "test_field");
        assert_eq!(result.0.description, "This is a field with metadata.");
        assert_eq!(result.0.default_value, Some("`\"test_value\"`".to_string()));
        assert_eq!(
            result.0.notes,
            Some(vec![
                "This is a note.".to_string(),
                "This is another note.".to_string()
            ])
        );
        assert_eq!(
            result.0.deprecated,
            Some("This field is deprecated.".to_string())
        );
        assert_eq!(
            result.0.toml_example,
            Some("key = \"value\"\nother = 123".to_string())
        );
    }

    #[test]
    fn test_parse_field_documentation_multiline_default() {
        let doc_text = r#"Multi-line field description.
---
@default: Derived from [`BurnchainConfig::mode`] ([`CHAIN_ID_MAINNET`] for `mainnet`,
  [`CHAIN_ID_TESTNET`] otherwise).
@notes:
  - Warning: Do not modify this unless you really know what you're doing."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        assert_eq!(result.0.name, "test_field");
        assert_eq!(result.0.description, "Multi-line field description.");
        assert!(result.0.default_value.is_some());
        let default_val = result.0.default_value.unwrap();
        assert!(default_val.contains("Derived from"));
        assert!(default_val.contains("CHAIN_ID_MAINNET"));
        assert_eq!(
            result.0.notes,
            Some(vec![
                "Warning: Do not modify this unless you really know what you're doing.".to_string()
            ])
        );
    }

    #[test]
    fn test_parse_field_documentation_multiline_notes() {
        let doc_text = r#"Field with multi-line notes.
---
@notes:
  - This is a single line note.
  - This is a multi-line note that
    spans across multiple lines
    and should be treated as one note.
  - Another single line note.
  - Final multi-line note that also
    continues on the next line."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();
        let (field_doc, _) = result;

        assert_eq!(field_doc.name, "test_field");
        assert_eq!(field_doc.description, "Field with multi-line notes.");

        let notes = field_doc.notes.expect("Should have notes");
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[0], "This is a single line note.");
        assert_eq!(
            notes[1],
            "This is a multi-line note that spans across multiple lines and should be treated as one note."
        );
        assert_eq!(notes[2], "Another single line note.");
        assert_eq!(
            notes[3],
            "Final multi-line note that also continues on the next line."
        );
    }

    #[test]
    fn test_parse_field_documentation_multiline_notes_mixed_bullets() {
        let doc_text = r#"Field with mixed bullet styles.
---
@notes:
  - First note with dash.
  * Second note with asterisk
    that continues.
  - Third note with dash again
    and multiple continuation lines
    should all be joined together."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();
        let (field_doc, _) = result;

        let notes = field_doc.notes.expect("Should have notes");
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0], "First note with dash.");
        assert_eq!(notes[1], "Second note with asterisk that continues.");
        assert_eq!(
            notes[2],
            "Third note with dash again and multiple continuation lines should all be joined together."
        );
    }

    #[test]
    fn test_parse_field_documentation_notes_with_empty_lines() {
        let doc_text = r#"Field with notes that have empty lines.
---
@notes:
  - First note.

  - Second note after empty line
    with continuation.

  - Third note after another empty line."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();
        let (field_doc, _) = result;

        let notes = field_doc.notes.expect("Should have notes");
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0], "First note.");
        assert_eq!(notes[1], "Second note after empty line with continuation.");
        assert_eq!(notes[2], "Third note after another empty line.");
    }

    #[test]
    fn test_parse_field_documentation_notes_with_intralinks() {
        let doc_text = r#"Field with notes containing intralinks.
---
@notes:
  - If [`SomeConfig::field`] is `true`, the node will
    use the default estimator.
  - See [`CONSTANT_VALUE`] for details."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();
        let (field_doc, referenced_constants) = result;

        let notes = field_doc.notes.expect("Should have notes");
        assert_eq!(notes.len(), 2);
        assert_eq!(
            notes[0],
            "If [`SomeConfig::field`] is `true`, the node will use the default estimator."
        );
        assert_eq!(notes[1], "See [`CONSTANT_VALUE`] for details.");

        // Check that constants were collected
        assert!(referenced_constants.contains("CONSTANT_VALUE"));
    }

    #[test]
    fn test_extract_annotation_basic() {
        let metadata = "@default: `\"test\"`\n@notes: Some notes here.";

        let default = extract_annotation(metadata, "default");
        let notes = extract_annotation(metadata, "notes");
        let missing = extract_annotation(metadata, "missing");

        assert_eq!(default, Some("`\"test\"`".to_string()));
        assert_eq!(notes, Some("Some notes here.".to_string()));
        assert_eq!(missing, None);
    }

    #[test]
    fn test_extract_annotation_toml_example() {
        let metadata = r#"@toml_example: |
  key = "value"
  number = 42
  nested = { a = 1, b = 2 }"#;

        let result = extract_annotation(metadata, "toml_example");
        assert!(result.is_some());
        let toml = result.unwrap();
        assert!(toml.contains("key = \"value\""));
        assert!(toml.contains("number = 42"));
        assert!(toml.contains("nested = { a = 1, b = 2 }"));
    }

    #[test]
    fn test_extract_annotation_multiline() {
        let metadata = r#"@notes:
  - First note with important details.
  - Second note with more info.
@default: `None`"#;

        let notes = extract_annotation(metadata, "notes");
        let default = extract_annotation(metadata, "default");

        assert!(notes.is_some());
        let notes_text = notes.unwrap();
        assert!(notes_text.contains("First note"));
        assert!(notes_text.contains("Second note"));
        assert_eq!(default, Some("`None`".to_string()));
    }

    #[test]
    fn test_extract_struct_fields_from_mock_data() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": ["field_1", "field_2"]
                            }
                        }
                    }
                }
            },
            "field_1": {
                "name": "test_field",
                "docs": "A test field.\n---\n@default: `42`"
            },
            "field_2": {
                "name": "another_field",
                "docs": "Another field with notes.\n---\n@default: `\"hello\"`\n@notes:\n  - This is a note."
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let (fields, _referenced_constants) = extract_struct_fields(index, struct_item).unwrap();

        assert_eq!(fields.len(), 2);

        let first_field = &fields[0];
        assert_eq!(first_field.name, "test_field");
        assert_eq!(first_field.description, "A test field.");
        assert_eq!(first_field.default_value, Some("`42`".to_string()));

        let second_field = &fields[1];
        assert_eq!(second_field.name, "another_field");
        assert_eq!(second_field.description, "Another field with notes.");
        assert_eq!(second_field.default_value, Some("`\"hello\"`".to_string()));
        assert_eq!(
            second_field.notes,
            Some(vec!["This is a note.".to_string()])
        );
    }

    #[test]
    fn test_extract_struct_from_rustdoc_index() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "docs": "This is a test struct for configuration.",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": ["field_1"]
                            }
                        }
                    }
                }
            },
            "field_1": {
                "name": "config_field",
                "docs": "Configuration field.\n---\n@default: `\"default\"`"
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let result = extract_struct_from_rustdoc_index(index, "TestStruct", struct_item).unwrap();

        assert!(result.0.is_some());
        let struct_doc = result.0.unwrap();
        assert_eq!(struct_doc.name, "TestStruct");
        assert_eq!(
            struct_doc.description,
            Some("This is a test struct for configuration.".to_string())
        );
        assert_eq!(struct_doc.fields.len(), 1);
        assert_eq!(struct_doc.fields[0].name, "config_field");
    }

    #[test]
    fn test_extract_config_docs_from_rustdoc() {
        let mock_rustdoc = json!({
            "index": {
                "item_1": {
                    "name": "ConfigStruct",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": ["field_1"]
                                }
                            }
                        }
                    },
                    "docs": "A configuration struct."
                },
                "item_2": {
                    "name": "NonStruct",
                    "inner": {
                        "function": {}
                    }
                },
                "field_1": {
                    "name": "setting",
                    "docs": "A configuration setting.\n---\n@default: `true`"
                }
            }
        });

        let target_structs = Some(vec!["ConfigStruct".to_string()]);
        let result =
            extract_config_docs_from_rustdoc(&mock_rustdoc, &target_structs, HashMap::new())
                .unwrap();

        assert_eq!(result.structs.len(), 1);
        let struct_doc = &result.structs[0];
        assert_eq!(struct_doc.name, "ConfigStruct");
        assert_eq!(
            struct_doc.description,
            Some("A configuration struct.".to_string())
        );
        assert_eq!(struct_doc.fields.len(), 1);
    }

    #[test]
    fn test_extract_config_docs_filter_by_target() {
        let mock_rustdoc = json!({
            "index": {
                "item_1": {
                    "name": "WantedStruct",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": []
                                }
                            }
                        }
                    },
                    "docs": "Wanted struct."
                },
                "item_2": {
                    "name": "UnwantedStruct",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": []
                                }
                            }
                        }
                    },
                    "docs": "Unwanted struct."
                }
            }
        });

        let target_structs = Some(vec!["WantedStruct".to_string()]);
        let result =
            extract_config_docs_from_rustdoc(&mock_rustdoc, &target_structs, HashMap::new())
                .unwrap();

        assert_eq!(result.structs.len(), 1);
        assert_eq!(result.structs[0].name, "WantedStruct");
    }

    #[test]
    fn test_extract_config_docs_no_filter() {
        let mock_rustdoc = json!({
            "index": {
                "item_1": {
                    "name": "Struct1",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": []
                                }
                            }
                        }
                    },
                    "docs": "First struct."
                },
                "item_2": {
                    "name": "Struct2",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": []
                                }
                            }
                        }
                    },
                    "docs": "Second struct."
                }
            }
        });

        let result =
            extract_config_docs_from_rustdoc(&mock_rustdoc, &None, HashMap::new()).unwrap();

        assert_eq!(result.structs.len(), 2);
        let names: Vec<&str> = result.structs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Struct1"));
        assert!(names.contains(&"Struct2"));
    }

    #[test]
    fn test_parse_field_documentation_empty_notes() {
        let doc_text = r#"Field with empty notes.
---
@default: `None`
@notes:


@deprecated: Old field"#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        assert_eq!(result.0.name, "test_field");
        assert_eq!(result.0.description, "Field with empty notes.");
        assert_eq!(result.0.default_value, Some("`None`".to_string()));
        assert_eq!(result.0.notes, None); // Empty notes should result in None
        assert_eq!(result.0.deprecated, Some("Old field".to_string()));
    }

    #[test]
    fn test_parse_field_documentation_bullet_points_cleanup() {
        let doc_text = r#"Field with bullet notes.
---
@notes:
  - First bullet point
  * Second bullet point
  - Third bullet point"#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        assert_eq!(
            result.0.notes,
            Some(vec![
                "First bullet point".to_string(),
                "Second bullet point".to_string(),
                "Third bullet point".to_string()
            ])
        );
    }

    #[test]
    fn test_extract_annotation_edge_cases() {
        // Test with annotation at the end
        let metadata1 = "@default: `value`";
        assert_eq!(
            extract_annotation(metadata1, "default"),
            Some("`value`".to_string())
        );

        // Test with empty annotation
        let metadata2 = "@default:\n@notes: something";
        assert_eq!(extract_annotation(metadata2, "default"), None);

        // Test with annotation containing colons
        let metadata3 = "@notes: URL: https://example.com:8080/path";
        let notes = extract_annotation(metadata3, "notes");
        assert_eq!(
            notes,
            Some("URL: https://example.com:8080/path".to_string())
        );

        // Test with whitespace-only annotation
        let metadata_whitespace = "@default:      \n@notes: something";
        assert_eq!(
            extract_annotation(metadata_whitespace, "default"),
            None,
            "Annotation with only whitespace should be None"
        );

        // Test with annotation containing only newline
        let metadata_newline = "@default:\n@notes: something";
        assert_eq!(
            extract_annotation(metadata_newline, "default"),
            None,
            "Annotation with only newline should be None"
        );
    }

    #[test]
    fn test_extract_struct_fields_numeric_field_ids() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": [123, 456] // Numeric field IDs
                            }
                        }
                    }
                }
            },
            "123": {
                "name": "numeric_field",
                "docs": "Field with numeric ID.\n---\n@default: `0`"
            },
            "456": {
                "name": "another_numeric",
                "docs": "Another numeric field."
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let (fields, _referenced_constants) = extract_struct_fields(index, struct_item).unwrap();

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "numeric_field");
        assert_eq!(fields[1].name, "another_numeric");
    }

    #[test]
    fn test_extract_struct_fields_missing_field_data() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": ["missing_field", "present_field"]
                            }
                        }
                    }
                }
            },
            "present_field": {
                "name": "present",
                "docs": "This field exists."
            }
            // "missing_field" is intentionally not in the index
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let (fields, _referenced_constants) = extract_struct_fields(index, struct_item).unwrap();

        // Should only include the present field
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "present");
    }

    #[test]
    fn test_extract_config_docs_missing_index() {
        let invalid_rustdoc = json!({
            "not_index": {}
        });

        let result = extract_config_docs_from_rustdoc(&invalid_rustdoc, &None, HashMap::new());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing 'index' field")
        );
    }

    #[test]
    fn test_extract_struct_fields_no_documentation() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": ["field_1"]
                            }
                        }
                    }
                }
            },
            "field_1": {
                "name": "undocumented_field",
                "docs": ""  // Empty documentation
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let (fields, _referenced_constants) = extract_struct_fields(index, struct_item).unwrap();

        // Fields without documentation should be excluded
        assert_eq!(fields.len(), 0);
    }

    #[test]
    fn test_extract_struct_fields_malformed_structure() {
        let mock_index = json!({
            "struct_1": {
                "name": "TestStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "tuple": {}  // Not a "plain" struct
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let (fields, _referenced_constants) = extract_struct_fields(index, struct_item).unwrap();

        // Should handle malformed structures gracefully
        assert_eq!(fields.len(), 0);
    }

    #[test]
    fn test_parse_field_documentation_complex_annotations() {
        let doc_text = r#"Complex field with all annotation types and edge cases.

This description spans multiple lines
and includes various formatting.
---
@default: Dynamically determined.
  - If the `[miner]` section *is present* in the config file, the [`NodeConfig::seed`] is used.
  - If the `[miner]` section *is not present*, this is `None`, and mining operations will fail.
@notes:
  - **Warning:** This field requires careful configuration.
  - Only relevant if [`NodeConfig::miner`] is `true`.
  - Units: milliseconds.
@deprecated: Use `new_field` instead. This will be removed in version 2.0.
@toml_example: |
  # This is a comment
  [section]
  field = "value"

  # Another section
  [other_section]
  number = 42
  array = ["a", "b", "c"]"#;

        let result = parse_field_documentation(doc_text, "complex_field").unwrap();

        assert_eq!(result.0.name, "complex_field");
        assert!(result.0.description.contains("Complex field"));
        assert!(result.0.description.contains("multiple lines"));

        let default_val = result.0.default_value.unwrap();
        assert!(default_val.contains("Dynamically determined"));
        assert!(default_val.contains("NodeConfig::seed"));

        let notes = result.0.notes.unwrap();
        assert_eq!(notes.len(), 3);
        assert!(notes[0].contains("Warning"));
        assert!(notes[1].contains("Only relevant"));
        assert!(notes[2].contains("Units: milliseconds"));

        assert!(
            result
                .0
                .deprecated
                .unwrap()
                .contains("Use `new_field` instead")
        );

        let toml_example = result.0.toml_example.unwrap();
        assert!(toml_example.contains("# This is a comment"));
        assert!(toml_example.contains("[section]"));
        assert!(toml_example.contains("array = [\"a\", \"b\", \"c\"]"));
    }

    #[test]
    fn test_extract_annotation_overlapping_patterns() {
        let metadata = r#"@config_value: `"not_default"`
@default: `"actual_default"`
@notes_info: Some other annotation
@notes: Actual notes here
@deprecated_old: Old deprecation
@deprecated: Current deprecation"#;

        // Should extract the correct annotations, not get confused by similar names
        assert_eq!(
            extract_annotation(metadata, "default"),
            Some("`\"actual_default\"`".to_string())
        );
        assert_eq!(
            extract_annotation(metadata, "notes"),
            Some("Actual notes here".to_string())
        );
        assert_eq!(
            extract_annotation(metadata, "deprecated"),
            Some("Current deprecation".to_string())
        );

        // Should not find non-existent annotations
        assert_eq!(extract_annotation(metadata, "nonexistent"), None);
        assert_eq!(extract_annotation(metadata, "missing"), None);
    }

    #[test]
    fn test_extract_struct_from_rustdoc_index_no_fields_no_description() {
        let mock_index = json!({
            "struct_1": {
                "name": "EmptyStruct",
                "inner": {
                    "struct": {
                        "kind": {
                            "plain": {
                                "fields": []
                            }
                        }
                    }
                }
                // No "docs" field
            }
        });

        let index = mock_index.as_object().unwrap();
        let struct_item = &mock_index["struct_1"];

        let result = extract_struct_from_rustdoc_index(index, "EmptyStruct", struct_item).unwrap();

        // Should return None for structs with no fields and no description
        assert!(result.0.is_none());
    }

    #[test]
    fn test_parse_field_documentation_only_description() {
        let doc_text = "Just a simple description with no metadata separator.";
        let result = parse_field_documentation(doc_text, "simple_field").unwrap();

        assert_eq!(result.0.name, "simple_field");
        assert_eq!(
            result.0.description,
            "Just a simple description with no metadata separator."
        );
        assert_eq!(result.0.default_value, None);
        assert_eq!(result.0.notes, None);
        assert_eq!(result.0.deprecated, None);
        assert_eq!(result.0.toml_example, None);
    }

    #[test]
    fn test_package_to_library_name_mapping() {
        // Test the logic inside generate_rustdoc_json for mapping package names to library names
        // We can't easily test generate_rustdoc_json directly since it runs external commands,
        // but we can test the mapping logic

        // Test the special case for stackslib
        let lib_name = match "stackslib" {
            "stackslib" => "blockstack_lib".to_string(),
            pkg => pkg.replace('-', "_"),
        };
        assert_eq!(lib_name, "blockstack_lib");

        // Test normal package names with hyphens
        let lib_name = match "config-docs-generator" {
            "stackslib" => "blockstack_lib".to_string(),
            pkg => pkg.replace('-', "_"),
        };
        assert_eq!(lib_name, "config_docs_generator");

        // Test package name without hyphens
        let lib_name = match "normalpackage" {
            "stackslib" => "blockstack_lib".to_string(),
            pkg => pkg.replace('-', "_"),
        };
        assert_eq!(lib_name, "normalpackage");
    }

    #[test]
    fn test_find_constant_references() {
        // Test finding constant references in text
        let text1 = "This field uses [`DEFAULT_VALUE`] as default.";
        let constants1 = find_constant_references(text1);
        assert_eq!(constants1.len(), 1);
        assert!(constants1.contains("DEFAULT_VALUE"));

        // Test multiple constants
        let text2 = "Uses [`CONST_A`] and [`CONST_B`] values.";
        let constants2 = find_constant_references(text2);
        assert_eq!(constants2.len(), 2);
        assert!(constants2.contains("CONST_A"));
        assert!(constants2.contains("CONST_B"));

        // Test no constants
        let text3 = "This text has no constant references.";
        let constants3 = find_constant_references(text3);
        assert_eq!(constants3.len(), 0);

        // Test mixed content
        let text4 =
            "Field uses [`MY_CONSTANT`] and links to [`SomeStruct::field`] but not `lowercase`.";
        let constants4 = find_constant_references(text4);
        assert_eq!(constants4.len(), 1);
        assert!(constants4.contains("MY_CONSTANT"));
        assert!(!constants4.contains("SomeStruct::field")); // Should not match struct::field patterns
        assert!(!constants4.contains("lowercase")); // Should not match lowercase
    }

    #[test]
    fn test_resolve_constant_reference() {
        // Create mock rustdoc index with a constant
        let mock_index = serde_json::json!({
            "const_1": {
                "name": "TEST_CONSTANT",
                "inner": {
                    "constant": {
                        "expr": "42",
                        "type": "u32"
                    }
                }
            },
            "const_2": {
                "name": "STRING_CONST",
                "inner": {
                    "constant": {
                        "value": "\"hello\"",
                        "type": "&str"
                    }
                }
            },
            "not_const": {
                "name": "NotAConstant",
                "inner": {
                    "function": {}
                }
            }
        });

        let index = mock_index.as_object().unwrap();

        // Test resolving existing constant with expr field
        let result1 = resolve_constant_reference("TEST_CONSTANT", index);
        assert_eq!(result1, Some("42".to_string()));

        // Test resolving existing constant with value field
        let result2 = resolve_constant_reference("STRING_CONST", index);
        assert_eq!(result2, Some("\"hello\"".to_string()));

        // Test resolving non-existent constant
        let result3 = resolve_constant_reference("NONEXISTENT", index);
        assert_eq!(result3, None);

        // Test resolving non-constant item
        let result4 = resolve_constant_reference("NotAConstant", index);
        assert_eq!(result4, None);
    }

    #[test]
    fn test_resolve_computed_constant() {
        // Test computed constants that have "_" in expr and actual value in value field
        let mock_index = serde_json::json!({
            "computed_const": {
                "name": "COMPUTED_CONSTANT",
                "inner": {
                    "constant": {
                        "const": {
                            "expr": "_",
                            "value": "402_653_196u32",
                            "is_literal": false
                        },
                        "type": {
                            "primitive": "u32"
                        }
                    }
                }
            },
            "literal_const": {
                "name": "LITERAL_CONSTANT",
                "inner": {
                    "constant": {
                        "const": {
                            "expr": "100",
                            "value": "100u32",
                            "is_literal": true
                        },
                        "type": {
                            "primitive": "u32"
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();

        // Test resolving computed constant - should get the value without type suffix
        let result1 = resolve_constant_in_index("COMPUTED_CONSTANT", index);
        assert_eq!(result1, Some("402_653_196".to_string()));

        // Test resolving literal constant - should get expr which is clean
        let result2 = resolve_constant_in_index("LITERAL_CONSTANT", index);
        assert_eq!(result2, Some("100".to_string()));
    }

    #[test]
    fn test_parse_field_documentation_with_constants() {
        let doc_text = r#"This field uses [`DEFAULT_TIMEOUT`] milliseconds.
---
@default: [`DEFAULT_VALUE`]
@notes:
  - See [`MAX_RETRIES`] for retry limit.
  - Warning about [`DEPRECATED_CONST`]."#;

        let result = parse_field_documentation(doc_text, "test_field").unwrap();

        // Check that constants were collected
        assert_eq!(result.1.len(), 4);
        assert!(result.1.contains("DEFAULT_TIMEOUT"));
        assert!(result.1.contains("DEFAULT_VALUE"));
        assert!(result.1.contains("MAX_RETRIES"));
        assert!(result.1.contains("DEPRECATED_CONST"));

        // Check that normal parsing still works
        assert_eq!(result.0.name, "test_field");
        assert!(result.0.description.contains("DEFAULT_TIMEOUT"));
        assert!(result.0.default_value.is_some());
        assert!(result.0.notes.is_some());
    }

    #[test]
    fn test_extract_config_docs_with_constants() {
        let mock_rustdoc = serde_json::json!({
            "index": {
                "struct_1": {
                    "name": "TestStruct",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": ["field_1"]
                                }
                            }
                        }
                    },
                    "docs": "Struct that uses [`STRUCT_CONSTANT`]."
                },
                "field_1": {
                    "name": "test_field",
                    "docs": "Field using [`FIELD_CONSTANT`].\n---\n@default: [`DEFAULT_CONST`]"
                },
                "const_1": {
                    "name": "STRUCT_CONSTANT",
                    "inner": {
                        "constant": {
                            "expr": "100"
                        }
                    }
                },
                "const_2": {
                    "name": "FIELD_CONSTANT",
                    "inner": {
                        "constant": {
                            "value": "\"test\""
                        }
                    }
                },
                "const_3": {
                    "name": "DEFAULT_CONST",
                    "inner": {
                        "constant": {
                            "expr": "42"
                        }
                    }
                }
            }
        });

        let target_structs = Some(vec!["TestStruct".to_string()]);
        let result =
            extract_config_docs_from_rustdoc(&mock_rustdoc, &target_structs, HashMap::new());
        assert!(result.is_ok());

        let config_docs = result.unwrap();
        assert_eq!(config_docs.structs.len(), 1);
        let struct_doc = &config_docs.structs[0];
        assert_eq!(struct_doc.name, "TestStruct");
        assert_eq!(struct_doc.fields.len(), 1);
        assert_eq!(struct_doc.fields[0].name, "test_field");
        assert_eq!(
            struct_doc.fields[0].default_value,
            Some("[`DEFAULT_CONST`]".to_string())
        );

        // Check that constants were resolved
        assert_eq!(config_docs.referenced_constants.len(), 3);
        assert_eq!(
            config_docs.referenced_constants.get("STRUCT_CONSTANT"),
            Some(&Some("100".to_string()))
        );
        assert_eq!(
            config_docs.referenced_constants.get("FIELD_CONSTANT"),
            Some(&Some("\"test\"".to_string()))
        );
        assert_eq!(
            config_docs.referenced_constants.get("DEFAULT_CONST"),
            Some(&Some("42".to_string()))
        );
    }

    #[test]
    fn test_extract_config_docs_empty_target_structs() {
        // Test with empty target structs list
        let rustdoc_json = json!({
            "index": {},
            "format_version": 1
        });

        let result = extract_config_docs_from_rustdoc(&rustdoc_json, &Some(vec![]), HashMap::new());
        assert!(result.is_ok());

        let config_docs = result.unwrap();
        assert_eq!(config_docs.structs.len(), 0);
    }

    #[test]
    fn test_extract_field_type_info_resolved_path() {
        // Test extracting type info from resolved_path structure
        let mock_index = json!({
            "config_struct_id": {
                "name": "BurnchainConfigFile",
                "inner": {
                    "struct": {}
                }
            }
        });

        let field_item = json!({
            "name": "burnchain",
            "inner": {
                "struct_field": {
                    "type": {
                        "resolved_path": {
                            "name": "BurnchainConfigFile"
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        assert_eq!(result, Some(("BurnchainConfigFile".to_string(), false)));
    }

    #[test]
    fn test_extract_field_type_info_vec_type() {
        // Test extracting type info from Vec<ConfigStruct> structure
        let mock_index = json!({
            "config_struct_id": {
                "name": "EventObserverConfigFile",
                "inner": {
                    "struct": {}
                }
            }
        });

        let field_item = json!({
            "name": "events_observer",
            "inner": {
                "struct_field": {
                    "type": {
                        "generic": {
                            "name": "Vec",
                            "args": [{
                                "resolved_path": {
                                    "name": "EventObserverConfigFile"
                                }
                            }]
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        // The simplified version only supports the newer rustdoc JSON format with "path" and "args.angle_bracketed"
        // This test uses the older "generic" structure which is no longer supported
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_field_type_info_option_type() {
        // Test extracting type info from Option<ConfigStruct> structure
        let mock_index = json!({
            "config_struct_id": {
                "name": "MinerConfigFile",
                "inner": {
                    "struct": {}
                }
            }
        });

        let field_item = json!({
            "name": "miner",
            "inner": {
                "struct_field": {
                    "type": {
                        "generic": {
                            "name": "Option",
                            "args": [{
                                "resolved_path": {
                                    "name": "MinerConfigFile"
                                }
                            }]
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        // The simplified version only supports the newer rustdoc JSON format with "path" and "args.angle_bracketed"
        // This test uses the older "generic" structure which is no longer supported
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_field_type_info_option_vec_type() {
        // Test extracting type info from Option<Vec<ConfigStruct>> structure
        let mock_index = json!({
            "config_struct_id": {
                "name": "SomeConfigFile",
                "inner": {
                    "struct": {}
                }
            }
        });

        let field_item = json!({
            "name": "optional_list",
            "inner": {
                "struct_field": {
                    "type": {
                        "generic": {
                            "name": "Option",
                            "args": [{
                                "generic": {
                                    "name": "Vec",
                                    "args": [{
                                        "resolved_path": {
                                            "name": "SomeConfigFile"
                                        }
                                    }]
                                }
                            }]
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        // The simplified version only supports the newer rustdoc JSON format with "path" and "args.angle_bracketed"
        // This test uses the older "generic" structure which is no longer supported
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_field_type_info_non_config_struct() {
        // Test that the function accepts any struct that exists in the index
        let mock_index = json!({
            "regular_struct_id": {
                "name": "RegularStruct",
                "inner": {
                    "struct": {}
                }
            }
        });

        let field_item = json!({
            "name": "regular_field",
            "inner": {
                "struct_field": {
                    "type": {
                        "resolved_path": {
                            "name": "RegularStruct"
                        }
                    }
                }
            }
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        // Should now return the struct since it exists in the index (no hardcoded filtering)
        assert_eq!(result, Some(("RegularStruct".to_string(), false)));
    }

    #[test]
    fn test_extract_field_type_info_alternative_paths() {
        // Test alternative JSON paths for type information
        let mock_index = json!({
            "config_struct_id": {
                "name": "NodeConfig",
                "inner": {
                    "struct": {}
                }
            }
        });

        // Test field.inner.type path
        let field_item1 = json!({
            "name": "node",
            "inner": {
                "type": {
                    "resolved_path": {
                        "name": "NodeConfig"
                    }
                }
            }
        });

        // Test field.type path
        let field_item2 = json!({
            "name": "node",
            "type": {
                "resolved_path": {
                    "name": "NodeConfig"
                }
            }
        });

        let index = mock_index.as_object().unwrap();

        let result1 = extract_field_type_info(&field_item1, index);
        let result2 = extract_field_type_info(&field_item2, index);

        assert_eq!(result1, Some(("NodeConfig".to_string(), false)));
        assert_eq!(result2, Some(("NodeConfig".to_string(), false)));
    }

    #[test]
    fn test_extract_field_type_info_missing_type() {
        // Test field with no type information
        let mock_index = json!({});

        let field_item = json!({
            "name": "field_without_type",
            "docs": "Some field documentation"
        });

        let index = mock_index.as_object().unwrap();
        let result = extract_field_type_info(&field_item, index);

        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_type_for_struct_name_primitive_type() {
        // Test that primitive types are ignored
        let mock_index = json!({});
        let index = mock_index.as_object().unwrap();

        let primitive_type = json!({
            "primitive": "u32"
        });

        let result = parse_type_for_struct_name(&primitive_type, index);
        assert_eq!(result, None);
    }

    #[test]
    fn test_derive_config_structs_from_main_with_type_extraction() {
        // Test the complete derive function with simplified type extraction
        let mock_rustdoc = json!({
            "index": {
                "main_config_id": {
                    "name": "ConfigFile",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": ["burnchain_field_id", "node_field_id", "events_field_id"]
                                }
                            }
                        }
                    }
                },
                "burnchain_field_id": {
                    "name": "burnchain",
                    "inner": {
                        "struct_field": {
                            "type": {
                                "resolved_path": {
                                    "name": "BurnchainConfigFile"
                                }
                            }
                        }
                    }
                },
                "node_field_id": {
                    "name": "node",
                    "inner": {
                        "struct_field": {
                            "type": {
                                "resolved_path": {
                                    "name": "NodeConfigFile"
                                }
                            }
                        }
                    }
                },
                "events_field_id": {
                    "name": "events_observer",
                    "inner": {
                        "struct_field": {
                            "type": {
                                "generic": {
                                    "name": "Vec",
                                    "args": [{
                                        "resolved_path": {
                                            "name": "EventObserverConfigFile"
                                        }
                                    }]
                                }
                            }
                        }
                    }
                },
                "burnchain_config_id": {
                    "name": "BurnchainConfigFile",
                    "inner": {
                        "struct": {}
                    }
                },
                "node_config_id": {
                    "name": "NodeConfigFile",
                    "inner": {
                        "struct": {}
                    }
                },
                "events_config_id": {
                    "name": "EventObserverConfigFile",
                    "inner": {
                        "struct": {}
                    }
                }
            }
        });

        let result = derive_config_structs_from_main(&mock_rustdoc, "ConfigFile");
        assert!(result.is_ok());

        let (structs, mappings) = result.unwrap();

        // With simplified logic, only the direct resolved_path structures work
        // The "generic" structure for events_observer won't be parsed
        assert_eq!(structs.len(), 2);
        assert!(structs.contains(&"BurnchainConfigFile".to_string()));
        assert!(structs.contains(&"NodeConfigFile".to_string()));

        assert_eq!(mappings.len(), 2);
        assert_eq!(
            mappings.get("[burnchain]"),
            Some(&"BurnchainConfigFile".to_string())
        );
        assert_eq!(mappings.get("[node]"), Some(&"NodeConfigFile".to_string()));
    }

    #[test]
    fn test_derive_config_structs_from_main_with_ignored_field() {
        // Test that fields with @ignore annotation are skipped
        let mock_rustdoc = json!({
            "index": {
                "main_config_id": {
                    "name": "ConfigFile",
                    "inner": {
                        "struct": {
                            "kind": {
                                "plain": {
                                    "fields": ["normal_field_id", "ignored_field_id"]
                                }
                            }
                        }
                    }
                },
                "normal_field_id": {
                    "name": "normal",
                    "docs": "Normal field documentation",
                    "inner": {
                        "struct_field": {
                            "type": {
                                "resolved_path": {
                                    "name": "NormalConfigFile"
                                }
                            }
                        }
                    }
                },
                "ignored_field_id": {
                    "name": "ignored",
                    "docs": "Field documentation\n@ignore",
                    "inner": {
                        "struct_field": {
                            "type": {
                                "resolved_path": {
                                    "name": "IgnoredConfigFile"
                                }
                            }
                        }
                    }
                },
                "normal_config_id": {
                    "name": "NormalConfigFile",
                    "inner": {
                        "struct": {}
                    }
                },
                "ignored_config_id": {
                    "name": "IgnoredConfigFile",
                    "inner": {
                        "struct": {}
                    }
                }
            }
        });

        let result = derive_config_structs_from_main(&mock_rustdoc, "ConfigFile");
        assert!(result.is_ok());

        let (structs, mappings) = result.unwrap();

        // Should only include the normal field, not the ignored one
        assert_eq!(structs.len(), 1);
        assert!(structs.contains(&"NormalConfigFile".to_string()));
        assert!(!structs.contains(&"IgnoredConfigFile".to_string()));

        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings.get("[normal]"),
            Some(&"NormalConfigFile".to_string())
        );
        assert!(!mappings.contains_key("[ignored]"));
    }
}
