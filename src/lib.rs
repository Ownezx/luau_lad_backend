//! Native Luau (`.d.luau`) definition-file backend for BMS Language Agnostic
//! Definition (LAD) files.
//!
//! BMS 0.24 models script APIs as importable namespaces and records generic
//! parameters, receiver information, type values, and enum-variant functions
//! explicitly. This backend preserves that model when producing Luau
//! definitions for `luau-lsp`.
//!
//! `import_namespace` is emitted as a single global whose type is an
//! intersection of namespace-specific function signatures. Luau uses
//! intersections to represent callable overloads; emitting several
//! `declare function import_namespace(...)` statements does not reliably
//! create overloads and can leave calls typed as `any`.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use ladfile::{
    LadArgument, LadFile, LadFilePlugin, LadFunction, LadNamedType, LadTypeId, LadTypeKind,
    LadTypeLayout, LadVariant, ReflectionPrimitiveKind,
};

/// Render a parsed LAD 0.24 file as Luau definition-file source.
pub fn lad_to_luau(lad: &LadFile) -> String {
    lad_to_luau_with(lad, &Options::default())
}

/// Render a parsed LAD file with custom output options.
pub fn lad_to_luau_with(lad: &LadFile, options: &Options) -> String {
    Converter::new(lad, options).render()
}

/// Options controlling optional generated aliases.
#[derive(Clone, Debug)]
pub struct Options {
    /// Emit `*Init` aliases for constructor payloads, including required/defaulted
    /// field information. Enabled by default.
    pub init_types: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { init_types: true }
    }
}

/// `luau-lsp analyze` inference limits suitable for large Bevy registries.
pub const FFLAGS: [(&str, &str); 5] = [
    ("LuauTypeInferIterationLimit", "10000000"),
    ("LuauTarjanChildLimit", "1000000"),
    ("LuauTypeCloneIterationLimit", "10000000"),
    ("LuauSolverConstraintLimit", "100000"),
    ("LuauTypeInferRecursionLimit", "2000"),
];

/// Return the inference limits as `luau-lsp analyze` command-line arguments.
pub fn fflags_args() -> String {
    FFLAGS
        .iter()
        .map(|(flag, value)| format!("--flag:{flag}={value}\n"))
        .collect()
}

/// BMS LAD post-processor writing a Luau `.d.luau` file.
#[derive(Clone, Debug)]
pub struct LuauLadPlugin {
    /// File written beneath the generation output directory.
    pub filename: PathBuf,
    /// Optional file receiving [`FFLAGS`] in command-line format.
    pub fflags_filename: Option<PathBuf>,
    /// Rendering options.
    pub options: Options,
}

impl Default for LuauLadPlugin {
    fn default() -> Self {
        Self {
            filename: PathBuf::from("luau/bindings.d.luau"),
            fflags_filename: None,
            options: Options::default(),
        }
    }
}

impl LadFilePlugin for LuauLadPlugin {
    fn name(&self) -> &'static str {
        "Luau definition file generator"
    }

    fn run(&self, ladfile: &LadFile, output_dir: &Path) -> Result<(), Box<dyn Error>> {
        let definitions_path = output_dir.join(&self.filename);
        if let Some(parent) = definitions_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&definitions_path, lad_to_luau_with(ladfile, &self.options))?;

        if let Some(path) = &self.fflags_filename {
            let fflags_path = output_dir.join(path);
            if let Some(parent) = fflags_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(fflags_path, fflags_args())?;
        }
        Ok(())
    }
}

struct Converter<'a> {
    lad: &'a LadFile,
    names: HashMap<LadTypeId, String>,
    generic_names: HashMap<LadTypeId, String>,
    namespace_names: HashMap<String, String>,
    registration_types: HashSet<LadTypeId>,
    options: &'a Options,
}

impl<'a> Converter<'a> {
    fn new(lad: &'a LadFile, options: &'a Options) -> Self {
        let mut used = HashSet::new();

        // Reserve generated top-level aliases before allocating reflected and
        // generic type names, so a user type cannot shadow the import callable.
        used.insert("ImportNamespace".to_string());

        let has_reflect_reference = lad.types.values().any(|def| {
            matches!(
                def.metadata.mapped_to_primitive_kind.as_ref(),
                Some(ReflectionPrimitiveKind::ReflectReference)
            )
        });
        if has_reflect_reference {
            used.insert("ReflectReference".to_string());
        }

        let mut names = HashMap::new();
        let mut ids: Vec<_> = lad.types.keys().cloned().collect();
        ids.sort_by(|a, b| {
            lad.types[a]
                .identifier
                .cmp(&lad.types[b].identifier)
                .then_with(|| a.cmp(b))
        });
        for id in ids {
            let def = &lad.types[&id];
            let is_reflect_reference = matches!(
                def.metadata.mapped_to_primitive_kind.as_ref(),
                Some(ReflectionPrimitiveKind::ReflectReference)
            );
            if def.metadata.mapped_to_primitive_kind.is_some() && !is_reflect_reference {
                continue;
            }
            let name = if is_reflect_reference {
                // Keep the proxy base as a declaration even though signatures
                // erase it to `any`; it owns the common reflection methods.
                "ReflectReference".to_string()
            } else {
                unique_name(&mut used, &def.identifier)
            };
            names.insert(id, name);
        }

        let mut generic_names = HashMap::new();
        let mut generic_ids: Vec<_> = lad.generic_types.keys().cloned().collect();
        generic_ids.sort_by(|a, b| {
            lad.generic_types[a]
                .identifier
                .cmp(&lad.generic_types[b].identifier)
                .then_with(|| a.cmp(b))
        });
        for id in generic_ids {
            let def = &lad.generic_types[&id];
            let name = unique_name(&mut used, &def.identifier);
            generic_names.insert(id, name);
        }

        let mut namespace_names = HashMap::new();
        let mut namespaces: Vec<_> = lad.namespaces.keys().map(|s| s.to_string()).collect();
        namespaces.sort();
        for ns in namespaces {
            let candidate = if ns.is_empty() {
                "Namespace_root".to_string()
            } else {
                format!("Namespace_{}", sanitize(&ns))
            };
            let name = unique_name(&mut used, &candidate);
            namespace_names.insert(ns, name);
        }

        // A 0.24 `Type<T>` parameter is erased to a concrete registration at
        // runtime. Use the metadata relation to render that as Registration<T>.
        let registration_types = lad
            .types
            .values()
            .filter_map(|def| def.metadata.registration.clone())
            .collect();

        Self {
            lad,
            names,
            generic_names,
            namespace_names,
            registration_types,
            options,
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("--!strict\n");
        out.push_str("-- AUTO-GENERATED from a BMS 0.24 LAD file by luau_lad_backend.\n");
        out.push_str("-- Do not edit by hand.\n");
        out.push_str(
            "--\n-- Large reflection registries may need increased Luau type-inference limits.\n",
        );
        out.push_str("-- Analyze with:\n");
        for (flag, value) in FFLAGS {
            out.push_str(&format!("--     --flag:{flag}={value}\n"));
        }
        out.push('\n');

        // Types and generic templates are declared before namespace tables.
        // The namespace import callable is emitted last, after all return types
        // have been declared.
        self.render_types(&mut out);
        self.render_generic_types(&mut out);
        self.render_global_functions(&mut out);
        self.render_namespaces(&mut out);
        self.render_import_namespace(&mut out);
        out
    }

    fn render_types(&self, out: &mut String) {
        let mut types: Vec<_> = self.names.iter().collect();
        types.sort_by(|a, b| a.1.cmp(b.1));

        for (id, name) in types {
            let def = &self.lad.types[id];
            if let Some(doc) = &def.documentation {
                push_doc(out, doc, "");
            }
            self.render_variant_alias(out, name, &def.layout);
            if self.options.init_types {
                self.render_init_alias(out, name, def);
            }
            self.render_static_type_alias(out, id, name, def);

            // Registration carriers are generic structural aliases. Other
            // reflected types use luau-lsp's definition-file `extern type`
            // declaration form, which supports nominal identity and inherited
            // members.
            if self.registration_types.contains(id) {
                self.render_registration_alias(out, name, def);
                continue;
            }
            let extends = if name != "ReflectReference" && def.metadata.is_reflect {
                " extends ReflectReference"
            } else {
                ""
            };
            out.push_str(&format!("declare extern type {name}{extends} with\n"));
            self.render_layout_members(out, &def.layout, def, name);

            // Instance methods belong to the nominal reflected type. Static
            // associated functions (including constructors such as `new`)
            // are already emitted on the corresponding `{name}Type` alias.
            let mut functions: Vec<_> = def
                .associated_functions
                .iter()
                .filter_map(|id| self.lad.functions.get(id))
                .filter(|function| function.receiver)
                .collect();
            functions.sort_by(|a, b| a.identifier.cmp(&b.identifier));
            for function in dedupe_overloads(functions) {
                self.render_type_function(out, name, function);
            }
            out.push_str("end\n\n");
        }
    }

    fn render_static_type_alias(
        &self,
        out: &mut String,
        type_id: &LadTypeId,
        name: &str,
        def: &ladfile::LadTypeDefinition,
    ) {
        // This is the type-table value returned by import_namespace(...). It is
        // deliberately separate from instances of the reflected type.
        let mut members = Vec::new();
        for function_id in &def.associated_functions {
            if let Some(function) = self.lad.functions.get(function_id) {
                if !function.receiver {
                    if let Some(member_name) = callable_name(&function.identifier) {
                        members.push(format!("{member_name}: {}", self.function_type(function)));
                    }
                }
            }
        }

        if let LadTypeLayout::Enum(variants) = &def.layout {
            for variant in variants {
                let raw_variant_name = variant_name(variant);
                let variant_type = format!("{name}_{}Type", sanitize(raw_variant_name));
                let mut variant_members = Vec::new();
                for (function_variant, function) in self.lad.variant_functions(type_id) {
                    if function_variant == raw_variant_name {
                        if let Some(member_name) = callable_name(&function.identifier) {
                            variant_members.push(format!(
                                "{member_name}: {}",
                                self.function_type_with_receiver(function, true)
                            ));
                        }
                    }
                }
                out.push_str(&format!("export type {variant_type} = {{"));
                if variant_members.is_empty() {
                    out.push_str("}\n");
                } else {
                    out.push('\n');
                    for member in variant_members {
                        out.push_str(&format!("\t{member},\n"));
                    }
                    out.push_str("}\n");
                }
                members.push(format!("{}: {variant_type}", field_key(raw_variant_name)));
            }
        }

        out.push_str(&format!("export type {name}Type = {{"));
        if members.is_empty() {
            out.push_str("}\n");
        } else {
            out.push('\n');
            for member in members {
                out.push_str(&format!("\t{member},\n"));
            }
            out.push_str("}\n");
        }
    }

    fn render_generic_types(&self, out: &mut String) {
        let mut types: Vec<_> = self.generic_names.iter().collect();
        types.sort_by(|a, b| a.1.cmp(b.1));
        for (id, name) in types {
            let def = &self.lad.generic_types[id];
            if let Some(doc) = &def.documentation {
                push_doc(out, doc, "");
            }
            let params = def
                .params
                .iter()
                .map(|p| sanitize(p))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("export type {name}<{params}> = "));
            out.push_str(&self.generic_layout_type(&def.layout));
            out.push_str("\n\n");
        }
    }

    /// Render a generic registration carrier as a structural type alias.
    /// These are the `Registration<T>` values used alongside concrete type
    /// constructor tables in namespace members.
    fn render_registration_alias(
        &self,
        out: &mut String,
        name: &str,
        def: &ladfile::LadTypeDefinition,
    ) {
        out.push_str(&format!("export type {name}<T> = {{\n"));

        match &def.layout {
            LadTypeLayout::Opaque => {}
            LadTypeLayout::MonoVariant(variant) => {
                self.render_alias_variant_fields(out, variant);
            }
            LadTypeLayout::Enum(variants) => {
                // A registration table may expose fields from several enum
                // variants. Mark each field optional because it is not present
                // on every variant.
                let mut fields: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for variant in variants {
                    if let LadVariant::Struct {
                        fields: variant_fields,
                        ..
                    } = variant
                    {
                        for field in variant_fields {
                            fields
                                .entry(field.name.clone())
                                .or_default()
                                .push(self.kind(&field.type_));
                        }
                    }
                }
                for (field, types) in fields {
                    let ty = unique_types(types).join(" | ");
                    out.push_str(&format!(
                        "\t{}: {},\n",
                        field_key(&field),
                        optional_type(&ty)
                    ));
                }
            }
        }

        let mut functions: Vec<_> = def
            .associated_functions
            .iter()
            .filter_map(|id| self.lad.functions.get(id))
            .collect();
        functions.sort_by(|a, b| a.identifier.cmp(&b.identifier));
        for function in dedupe_overloads(functions) {
            let Some(member_name) = callable_name(&function.identifier) else {
                continue;
            };
            let signature = self.function_type_with_alias_receiver(function, name);
            out.push_str(&format!("\t{member_name}: {signature},\n"));
        }
        out.push_str("}\n");
    }

    fn render_alias_variant_fields(&self, out: &mut String, variant: &LadVariant) {
        if let LadVariant::Struct { fields, .. } = variant {
            for field in fields {
                out.push_str(&format!(
                    "\t{}: {},\n",
                    field_key(&field.name),
                    self.kind(&field.type_)
                ));
            }
        }
    }

    fn function_type_with_alias_receiver(&self, function: &LadFunction, owner: &str) -> String {
        let mut args = function.arguments.iter();
        if function.receiver {
            let _ = args.next();
        }
        let args: Vec<_> = args.collect();
        let mut params = Vec::new();
        if function.receiver {
            params.push(format!("self: {owner}<T>"));
        }
        if !args.is_empty() {
            params.push(self.params(&args));
        }
        let ret = self.kind(&function.return_type.kind);
        format!(
            "{}({}) -> {ret}",
            generic_param_list(&function.generics),
            params.join(", ")
        )
    }

    fn generic_layout_type(&self, layout: &LadTypeLayout) -> String {
        match layout {
            LadTypeLayout::Opaque => "{}".to_string(),
            LadTypeLayout::MonoVariant(variant) => self.generic_variant_type(variant),
            LadTypeLayout::Enum(variants) => {
                if variants.is_empty() {
                    "{}".to_string()
                } else {
                    variants
                        .iter()
                        .map(|variant| self.generic_variant_type(variant))
                        .collect::<Vec<_>>()
                        .join(" | ")
                }
            }
        }
    }

    fn generic_variant_type(&self, variant: &LadVariant) -> String {
        match variant {
            LadVariant::Struct { fields, .. } => {
                let members = fields
                    .iter()
                    .map(|field| format!("{}: {}", field_key(&field.name), self.kind(&field.type_)))
                    .collect::<Vec<_>>();
                format!("{{ {} }}", members.join(", "))
            }
            LadVariant::TupleStruct { .. } | LadVariant::Unit { .. } => "{}".to_string(),
        }
    }

    fn render_layout_members(
        &self,
        out: &mut String,
        layout: &LadTypeLayout,
        def: &ladfile::LadTypeDefinition,
        owner: &str,
    ) {
        match layout {
            LadTypeLayout::Opaque => {}
            LadTypeLayout::MonoVariant(variant) => {
                self.render_variant_fields(out, variant, def, None, owner);
            }
            LadTypeLayout::Enum(variants) => {
                let mut fields: std::collections::BTreeMap<String, Vec<String>> =
                    std::collections::BTreeMap::new();
                for variant in variants {
                    if let LadVariant::Struct {
                        fields: variant_fields,
                        ..
                    } = variant
                    {
                        for field in variant_fields {
                            fields
                                .entry(field.name.clone())
                                .or_default()
                                .push(self.kind(&field.type_));
                        }
                    }
                }
                for (name, types) in fields {
                    let ty = unique_types(types).join(" | ");
                    out.push_str(&format!(
                        "\t{}: {},\n",
                        field_key(&name),
                        optional_type(&ty)
                    ));
                }
            }
        }
        let _ = owner;
    }

    fn render_variant_fields(
        &self,
        out: &mut String,
        variant: &LadVariant,
        def: &ladfile::LadTypeDefinition,
        variant_name: Option<&str>,
        owner: &str,
    ) {
        match variant {
            LadVariant::Struct { fields, .. } => {
                for field in fields {
                    let key = field_key(&field.name);
                    out.push_str(&format!("\t{key}: {}\n", self.kind(&field.type_)));
                }
            }
            LadVariant::TupleStruct { .. } => {}
            LadVariant::Unit { .. } => {}
        }
        let _ = (def, variant_name, owner);
    }

    fn render_variant_alias(&self, out: &mut String, name: &str, layout: &LadTypeLayout) {
        let LadTypeLayout::Enum(variants) = layout else {
            return;
        };
        if variants.is_empty() {
            return;
        }
        let values = variants
            .iter()
            .map(|v| format!("\"{}\"", escape_luau_string(variant_name(v))))
            .collect::<Vec<_>>()
            .join(" | ");
        out.push_str(&format!("export type {name}Variant = {values}\n"));
    }

    fn render_init_alias(&self, out: &mut String, name: &str, def: &ladfile::LadTypeDefinition) {
        match &def.layout {
            LadTypeLayout::Opaque => return,
            LadTypeLayout::MonoVariant(variant) => {
                let fields = self.fields_for_variant(def, variant, None);
                if fields.is_empty() {
                    return;
                }
                out.push_str(&format!("export type {name}Init = {{\n"));
                for (key, ty, optional) in fields {
                    out.push_str(&format!("\t{key}: {},\n", optional_type_if(&ty, optional)));
                }
                out.push_str("}\n");
            }
            LadTypeLayout::Enum(variants) => {
                if variants.is_empty() {
                    return;
                }
                out.push_str(&format!("export type {name}Init =\n"));
                for (i, variant) in variants.iter().enumerate() {
                    let variant_label = escape_luau_string(variant_name(variant));
                    let fields = self.fields_for_variant(def, variant, Some(variant_name(variant)));
                    let suffix = if i + 1 == variants.len() {
                        "\n"
                    } else {
                        " |\n"
                    };
                    if fields.is_empty() {
                        out.push_str(&format!("\t{{ variant: \"{variant_label}\" }}{suffix}"));
                    } else {
                        out.push_str(&format!("\t{{ variant: \"{variant_label}\", "));
                        for (index, (key, ty, optional)) in fields.iter().enumerate() {
                            if index > 0 {
                                out.push_str(", ");
                            }
                            out.push_str(&format!("{key}: {}", optional_type_if(ty, *optional)));
                        }
                        out.push_str(&format!(" }}{suffix}"));
                    }
                }
            }
        }
    }

    fn fields_for_variant(
        &self,
        def: &ladfile::LadTypeDefinition,
        variant: &LadVariant,
        variant_name: Option<&str>,
    ) -> Vec<(String, String, bool)> {
        match variant {
            LadVariant::Struct { fields, .. } => fields
                .iter()
                .map(|field| {
                    (
                        field_key(&field.name),
                        self.kind(&field.type_),
                        def.field_has_default(variant_name, field.has_default),
                    )
                })
                .collect(),
            LadVariant::TupleStruct { .. } => Vec::new(),
            LadVariant::Unit { .. } => Vec::new(),
        }
    }

    fn render_type_function(&self, out: &mut String, owner: &str, function: &LadFunction) {
        let Some(name) = callable_name(&function.identifier) else {
            log::warn!(
                "skipping `{}` on `{owner}`: not a usable Luau identifier",
                function.identifier
            );
            return;
        };
        if let Some(doc) = &function.documentation {
            push_doc(out, doc, "\t");
        }
        self.push_arg_docs(out, function, "\t");
        let mut args = function.arguments.iter();
        if function.receiver {
            let _ = args.next();
        }
        let args: Vec<_> = args.collect();
        let params = self.params(&args);
        let ret = self.kind(&function.return_type.kind);
        let generic_params = generic_param_list(&function.generics);
        if function.receiver {
            let comma = if params.is_empty() { "" } else { ", " };
            out.push_str(&format!(
                "\tfunction {name}{generic_params}(self{comma}{params}): {ret}\n"
            ));
        } else {
            out.push_str(&format!("\t{name}: {generic_params}({params}) -> {ret}\n"));
        }
    }

    fn render_global_functions(&self, out: &mut String) {
        let mut functions: Vec<_> = self
            .lad
            .builtin_functions()
            .map(|(_, function)| function)
            .collect();
        functions.sort_by(|a, b| a.identifier.cmp(&b.identifier));
        for function in dedupe_overloads(functions) {
            if function.identifier.as_ref() == "import_namespace" {
                continue;
            }
            let Some(name) = callable_name(&function.identifier) else {
                continue;
            };
            if let Some(doc) = &function.documentation {
                push_doc(out, doc, "");
            }
            self.push_arg_docs(out, function, "");
            let params = self.params(&function.arguments.iter().collect::<Vec<_>>());
            let ret = self.kind(&function.return_type.kind);
            let generic_params = generic_param_list(&function.generics);
            out.push_str(&format!(
                "declare function {name}{generic_params}({params}): {ret}\n\n"
            ));
        }
    }

    fn render_namespaces(&self, out: &mut String) {
        let mut namespaces: Vec<_> = self.lad.namespaces.iter().collect();
        namespaces.sort_by(|a, b| a.0.cmp(b.0));

        for (namespace, members) in namespaces {
            let ns_name = &self.namespace_names[namespace.as_ref()];
            out.push_str(&format!("export type {ns_name} = {{\n"));

            let mut types: Vec<_> = members.types.iter().collect();
            types.sort_by(|a, b| a.0.cmp(b.0));
            for (script_name, type_id) in types {
                let ty = self.namespace_type_value(type_id);
                out.push_str(&format!("\t{}: {ty},\n", field_key(script_name)));
            }

            let mut values: Vec<_> = members.values.iter().collect();
            values.sort_by(|a, b| a.0.cmp(b.0));
            for (script_name, instance) in values {
                let ty = self.kind(&instance.type_kind);
                out.push_str(&format!("\t{}: {ty},\n", field_key(script_name)));
            }

            let mut functions: Vec<_> = members
                .functions
                .iter()
                .filter_map(|(name, id)| self.lad.functions.get(id).map(|f| (name, f)))
                .collect();
            functions.sort_by(|a, b| a.0.cmp(b.0));
            for (script_name, function) in functions {
                if let Some(doc) = &function.documentation {
                    push_doc(out, doc, "\t");
                }
                self.push_arg_docs(out, function, "\t");
                let signature = self.function_type(function);
                out.push_str(&format!("\t{}: {signature},\n", field_key(script_name)));
            }
            out.push_str("}\n\n");
        }
    }

    /// Emit one callable type containing all statically-known namespace imports.
    ///
    /// Luau models overloadable callables as an intersection of function types.
    /// A literal call such as `import_namespace("odp")` therefore resolves to
    /// `Namespace_odp`, while the final `string -> any` arm keeps dynamic/runtime
    /// namespaces (for example `script.game`) usable without pretending that
    /// their members are statically known.
    fn render_import_namespace(&self, out: &mut String) {
        let mut namespaces: Vec<_> = self.namespace_names.iter().collect();
        namespaces.sort_by(|a, b| a.0.cmp(b.0));

        let mut signatures = Vec::with_capacity(namespaces.len() + 1);
        for (namespace, type_name) in namespaces {
            signatures.push(format!(
                "((namespace: \"{}\") -> {type_name})",
                escape_luau_string(namespace)
            ));
        }

        // BMS supports script-created namespaces that cannot be known from a
        // static LAD file. Keep those imports legal, but restrict them to `any`.
        signatures.push("((namespace: string) -> any)".to_string());

        out.push_str("export type ImportNamespace =\n");
        for (index, signature) in signatures.iter().enumerate() {
            if index == 0 {
                out.push_str("\t");
            } else {
                out.push_str("\t& ");
            }
            out.push_str(signature);
            out.push('\n');
        }
        out.push_str("\ndeclare import_namespace: ImportNamespace\n\n");
    }

    fn namespace_type_value(&self, type_id: &LadTypeId) -> String {
        let Some(def) = self.lad.types.get(type_id) else {
            return "any".to_string();
        };
        let Some(name) = self.names.get(type_id) else {
            return def
                .metadata
                .mapped_to_primitive_kind
                .as_ref()
                .map(|p| primitive(p).to_string())
                .unwrap_or_else(|| "any".to_string());
        };

        if let Some(registration) = &def.metadata.registration {
            let reg = self.type_name(registration);
            format!("{reg}<{name}> & {name}Type")
        } else {
            format!("{name}Type")
        }
    }

    fn function_type(&self, function: &LadFunction) -> String {
        self.function_type_with_receiver(function, false)
    }

    fn function_type_with_receiver(&self, function: &LadFunction, keep_receiver: bool) -> String {
        let mut args = function.arguments.iter();
        if function.receiver && !keep_receiver {
            let _ = args.next();
        }
        let args: Vec<_> = args.collect();
        let params = self.params(&args);
        let ret = self.kind(&function.return_type.kind);
        format!(
            "{}({params}) -> {ret}",
            generic_param_list(&function.generics)
        )
    }

    fn push_arg_docs(&self, out: &mut String, function: &LadFunction, indent: &str) {
        for (index, arg) in function.arguments.iter().enumerate() {
            if let Some(doc) = arg.documentation.as_deref() {
                let name = arg_name(arg, index);
                push_doc(
                    out,
                    &format!("@param {name} - {}", flatten_doc(doc)),
                    indent,
                );
            }
        }
        if let Some(doc) = function.return_type.documentation.as_deref() {
            push_doc(out, &format!("@return {}", flatten_doc(doc)), indent);
        }
    }

    fn params(&self, args: &[&LadArgument]) -> String {
        let has_untyped_rest = args
            .last()
            .is_some_and(|arg| matches!(&arg.kind, LadTypeKind::UntypedTuple));
        let fixed_args = if has_untyped_rest {
            &args[..args.len() - 1]
        } else {
            args
        };
        let mut params = fixed_args
            .iter()
            .enumerate()
            .map(|(index, arg)| format!("{}: {}", arg_name(arg, index), self.kind(&arg.kind)))
            .collect::<Vec<_>>();
        if has_untyped_rest {
            params.push("...any".to_string());
        }
        params.join(", ")
    }

    fn kind(&self, kind: &LadTypeKind) -> String {
        match kind {
            LadTypeKind::Ref(named) | LadTypeKind::Mut(named) | LadTypeKind::Val(named) => {
                self.named_type(named)
            }
            LadTypeKind::Unknown(_) => "any".to_string(),
            LadTypeKind::Option(inner) => format!("{}?", self.kind(inner)),
            LadTypeKind::Vec(inner)
            | LadTypeKind::HashSet(inner)
            | LadTypeKind::Array(inner, _) => {
                format!("{{ {} }}", self.kind(inner))
            }
            LadTypeKind::HashMap(key, value) => {
                format!("{{ [{}]: {} }}", self.kind(key), self.kind(value))
            }
            LadTypeKind::InteropResult(inner) => self.kind(inner),
            LadTypeKind::Tuple(items) => {
                let parts = unique_types(items.iter().map(|kind| self.kind(kind)));
                if parts.is_empty() {
                    "nil".to_string()
                } else {
                    format!("{{ {} }}", parts.join(" | "))
                }
            }
            LadTypeKind::UntypedTuple => "{ any }".to_string(),
            LadTypeKind::TupleWithRest { head, rest } => {
                let mut all = head.iter().map(|kind| self.kind(kind)).collect::<Vec<_>>();
                all.push(self.kind(rest));
                let all = unique_types(all);
                format!("{{ {} }}", all.join(" | "))
            }
            LadTypeKind::Primitive(p) => primitive(p).to_string(),
            LadTypeKind::Union(items) => {
                let items = unique_types(items.iter().map(|kind| self.kind(kind)));
                if items.is_empty() {
                    "any".to_string()
                } else {
                    items.join(" | ")
                }
            }
            LadTypeKind::Fields { of, variant } => self.fields_type(of, variant.as_deref()),
            LadTypeKind::Type { of, erased } => self.type_value_type(of, erased),
        }
    }

    fn named_type(&self, named: &LadNamedType) -> String {
        match named {
            LadNamedType::Id(id) => self.type_name(id),
            LadNamedType::Generic { generic_type, args } => {
                let name = self
                    .generic_names
                    .get(generic_type)
                    .cloned()
                    .or_else(|| {
                        self.lad
                            .generic_types
                            .get(generic_type)
                            .map(|d| sanitize(&d.identifier))
                    })
                    .unwrap_or_else(|| "any".to_string());
                let args = args
                    .iter()
                    .map(|arg| self.kind(arg))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}<{args}>")
            }
            LadNamedType::Param { param } => sanitize(param),
        }
    }

    fn type_name(&self, id: &LadTypeId) -> String {
        if let Some(primitive_kind) = self.lad.primitive_kind(id) {
            return primitive(primitive_kind).to_string();
        }
        self.names
            .get(id)
            .cloned()
            .or_else(|| self.generic_names.get(id).cloned())
            .unwrap_or_else(|| "any".to_string())
    }

    fn type_value_type(&self, of: &LadNamedType, erased: &LadTypeKind) -> String {
        let target = self.named_type(of);
        let registration = match erased {
            LadTypeKind::Ref(named) | LadTypeKind::Mut(named) | LadTypeKind::Val(named) => {
                Some(self.named_type(named))
            }
            _ => None,
        };
        if let Some(registration) = registration {
            let static_type = match of {
                LadNamedType::Id(id) => self
                    .names
                    .get(id)
                    .map(|name| format!(" & {name}Type"))
                    .unwrap_or_default(),
                _ => String::new(),
            };
            format!("{registration}<{target}>{static_type}")
        } else {
            target
        }
    }

    fn fields_type(&self, of: &LadNamedType, variant: Option<&str>) -> String {
        let (layout, optional_default) = match of {
            LadNamedType::Id(id) => match self.lad.types.get(id) {
                Some(def) => (
                    def.layout.clone(),
                    def.metadata
                        .default
                        .as_ref()
                        .is_some_and(|d| d.variant.as_deref() == variant),
                ),
                None => return "{ [string]: any }".to_string(),
            },
            LadNamedType::Generic { generic_type, args } => {
                let Some(def) = self.lad.generic_types.get(generic_type) else {
                    return "{ [string]: any }".to_string();
                };
                let mut layout = def.layout.clone();
                for (param, arg) in def.params.iter().zip(args) {
                    layout.replace_param(param, arg);
                }
                let default = self
                    .lad
                    .get_instantiation(generic_type, args)
                    .and_then(|inst| inst.metadata.default.as_ref())
                    .is_some_and(|d| d.variant.as_deref() == variant);
                (layout, default)
            }
            LadNamedType::Param { .. } => return "{ [string]: any }".to_string(),
        };
        let variants: Vec<&LadVariant> = match &layout {
            LadTypeLayout::Opaque => Vec::new(),
            LadTypeLayout::MonoVariant(variant) => vec![variant],
            LadTypeLayout::Enum(variants) => variants.iter().collect(),
        };
        let mut rendered = Vec::new();
        for item in variants {
            if variant.is_some_and(|wanted| variant_name(item) != wanted) {
                continue;
            }
            let fields = match item {
                LadVariant::Struct { fields, .. } => fields
                    .iter()
                    .map(|field| {
                        let optional = field.has_default || optional_default;
                        format!(
                            "{}: {}",
                            field_key(&field.name),
                            optional_type_if(&self.kind(&field.type_), optional)
                        )
                    })
                    .collect::<Vec<_>>(),
                LadVariant::TupleStruct { .. } => Vec::new(),
                LadVariant::Unit { .. } => Vec::new(),
            };
            rendered.push(format!("{{ {} }}", fields.join(", ")));
        }
        if rendered.is_empty() {
            "{}".to_string()
        } else {
            rendered.join(" | ")
        }
    }
}

fn unique_name(used: &mut HashSet<String>, raw: &str) -> String {
    let base = sanitize(raw);
    let base = if base.is_empty() {
        "Unknown".to_string()
    } else {
        base
    };
    let mut name = base.clone();
    while !used.insert(name.clone()) {
        name.push('_');
    }
    name
}

fn unique_types(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut result = Vec::new();
    for item in items {
        if !result.contains(&item) {
            result.push(item);
        }
    }
    result
}

/// Luau expresses an optional table property by making its value type
/// optional (`field: Value?`), not with TypeScript's `field?: Value` syntax.
fn optional_type(ty: &str) -> String {
    let ty = ty.trim();
    if ty.ends_with('?') {
        ty.to_string()
    } else if ty.contains(" | ") || ty.contains(" & ") {
        format!("({ty})?")
    } else {
        format!("{ty}?")
    }
}

fn optional_type_if(ty: &str, optional: bool) -> String {
    if optional {
        optional_type(ty)
    } else {
        ty.to_string()
    }
}

fn dedupe_overloads<'a>(functions: Vec<&'a LadFunction>) -> Vec<&'a LadFunction> {
    let mut seen = HashSet::new();
    functions
        .into_iter()
        .filter(|function| {
            let key = format!(
                "{}:{:?}:{:?}",
                function.identifier, function.arguments, function.return_type.kind
            );
            seen.insert(key)
        })
        .collect()
}

fn generic_param_list(generics: &[String]) -> String {
    if generics.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            generics
                .iter()
                .map(|g| sanitize(g))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn arg_name(arg: &LadArgument, index: usize) -> String {
    arg.name
        .as_deref()
        .map(sanitize)
        .filter(|name| !name.is_empty() && !is_reserved(name))
        .unwrap_or_else(|| format!("arg{}", index + 1))
}

fn flatten_doc(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn variant_name(variant: &LadVariant) -> &str {
    match variant {
        LadVariant::Struct { name, .. }
        | LadVariant::TupleStruct { name, .. }
        | LadVariant::Unit { name } => name,
    }
}

fn primitive(kind: &ReflectionPrimitiveKind) -> &'static str {
    use ReflectionPrimitiveKind::*;
    match kind {
        Bool => "boolean",
        Isize | I8 | I16 | I32 | I64 | I128 | Usize | U8 | U16 | U32 | U64 | U128 | F32 | F64 => {
            "number"
        }
        Char | Str | String | OsString | PathBuf => "string",
        _ => "any",
    }
}

const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue",
];

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

fn callable_name(raw: &str) -> Option<String> {
    let name = sanitize(raw);
    (!name.is_empty() && !is_reserved(&name)).then_some(name)
}

fn sanitize(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            result.push(ch);
        } else {
            result.push('_');
        }
    }
    if result.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        result.insert(0, '_');
    }
    result
}

fn field_key(raw: &str) -> String {
    let name = sanitize(raw);
    if name == raw && !is_reserved(&name) {
        name
    } else {
        format!("[\"{}\"]", escape_luau_string(raw))
    }
}

fn escape_luau_string(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn push_doc(out: &mut String, doc: &str, indent: &str) {
    for line in doc.lines() {
        out.push_str(indent);
        out.push_str("-- ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bms_024_example_file() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).expect("example LAD parses");
        assert!(ladfile::LAD_VERSION.starts_with("0.24"));
        let defs = lad_to_luau(&lad);

        assert!(defs.contains("declare function hello_world("), "{defs}");
        assert!(defs.contains("export type ImportNamespace ="), "{defs}");
        assert!(
            defs.contains("declare import_namespace: ImportNamespace"),
            "{defs}"
        );
        assert!(defs.contains("((namespace: string) -> any)"), "{defs}");
        assert!(defs.contains("export type Namespace_"), "{defs}");
        assert!(defs.contains("Variant ="), "{defs}");
        assert!(defs.contains("Init ="), "{defs}");
    }

    #[test]
    fn emits_a_single_intersection_typed_import_namespace() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let defs = lad_to_luau(&lad);

        assert_eq!(
            defs.matches("declare import_namespace:").count(),
            1,
            "{defs}"
        );
        assert!(
            !defs.contains("declare function import_namespace"),
            "{defs}"
        );
        assert!(defs.contains("export type ImportNamespace ="), "{defs}");

        let converter = Converter::new(&lad, &Options::default());
        for (namespace, expected_type) in &converter.namespace_names {
            let escaped = escape_luau_string(namespace);
            assert!(
                defs.contains(&format!("((namespace: \"{escaped}\") -> {expected_type})")),
                "missing typed import signature for namespace `{namespace}` in:\n{defs}"
            );
        }
    }

    #[test]
    fn import_namespace_output_is_deterministic() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let first = lad_to_luau(&lad);
        let second = lad_to_luau(&lad);
        assert_eq!(first, second);
    }

    #[test]
    fn preserves_generic_types_and_namespaced_imports() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let defs = lad_to_luau(&lad);
        for namespace in lad.namespaces.keys() {
            assert!(
                defs.contains(&format!("namespace: \"{}\"", escape_luau_string(namespace))),
                "{defs}"
            );
        }
        for generic in lad.generic_types.values() {
            assert!(
                defs.contains(&format!("export type {}<", sanitize(&generic.identifier))),
                "{defs}"
            );
        }
    }

    #[test]
    fn no_init_types_option_only_removes_payload_aliases() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let defs = lad_to_luau_with(&lad, &Options { init_types: false });
        assert!(!defs.contains("Init ="), "{defs}");
        assert!(defs.contains("declare extern type"), "{defs}");
        assert!(
            defs.contains("declare import_namespace: ImportNamespace"),
            "{defs}"
        );
    }

    #[test]
    fn fflags_are_rendered_one_per_line() {
        let args = fflags_args();
        assert_eq!(args.lines().count(), FFLAGS.len());
        assert!(args.contains("--flag:LuauTypeInferIterationLimit=10000000"));
    }

    #[test]
    fn generated_file_is_native_luau_not_luals_emmylua() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let defs = lad_to_luau(&lad);

        assert!(
            !defs.contains("---@"),
            "LuaLS annotations must not leak into Luau output"
        );
        assert!(
            !defs.contains("local namespace_"),
            "the generated Luau file should use typed namespace aliases, not LuaLS module locals"
        );
        assert!(defs.contains("declare extern type "), "{defs}");
        assert!(
            !defs.contains("?:"),
            "TypeScript-style optional keys are not valid Luau: {defs}"
        );
        assert!(!defs.contains("declare class "), "{defs}");
        assert!(defs.contains("export type Namespace_"), "{defs}");
    }

    #[test]
    fn reflected_types_use_luau_lsp_extern_type_declarations() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let defs = lad_to_luau(&lad);

        assert!(defs.contains("declare extern type "), "{defs}");
        assert!(defs.contains(" with\n"), "{defs}");
        assert!(!defs.contains("declare class "), "{defs}");
        assert!(
            !defs.contains("declare extern type ") || defs.contains("end\n"),
            "{defs}"
        );
    }

    #[test]
    fn optional_table_properties_use_luau_value_optional_syntax() {
        assert_eq!(optional_type("Vec3A"), "Vec3A?");
        assert_eq!(optional_type("Vec3A?"), "Vec3A?");
        assert_eq!(optional_type("number | string"), "(number | string)?");
        assert_eq!(optional_type_if("Vec3A", false), "Vec3A");
    }

    #[test]
    fn plugin_creates_nested_output_directories() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "luau_lad_backend_nested_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let plugin = LuauLadPlugin {
            fflags_filename: Some(PathBuf::from("luau/fflags.txt")),
            ..Default::default()
        };
        plugin.run(&lad, &dir).unwrap();

        let defs_path = dir.join("luau/bindings.d.luau");
        let flags_path = dir.join("luau/fflags.txt");
        assert!(defs_path.is_file());
        assert!(flags_path.is_file());
        let generated = std::fs::read_to_string(defs_path).unwrap();
        assert!(generated.contains("--!strict"));
        assert!(generated.contains("declare import_namespace: ImportNamespace"));
        assert_eq!(std::fs::read_to_string(flags_path).unwrap(), fflags_args());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn field_keys_preserve_keywords_and_other_names() {
        assert_eq!(field_key("normal"), "normal");
        assert_eq!(field_key("end"), "[\"end\"]");
        assert_eq!(field_key("not-valid"), "[\"not-valid\"]");
    }
}
