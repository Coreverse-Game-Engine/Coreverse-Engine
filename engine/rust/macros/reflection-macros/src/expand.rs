//! Expansion of `#[derive(Reflect)]`.

use std::collections::HashSet;

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DataEnum, DataStruct, DeriveInput, Error, Fields, GenericParam, Generics, Index, LitStr,
    Path, Result, Type, parse_quote,
};

use crate::attrs::{self, ContainerAttrs, MetaAttrs};

/// Expands the derive for one type: the `Typed`, `Reflect` and kind-trait implementations.
pub(crate) fn derive(input: &DeriveInput) -> Result<TokenStream> {
    check_generics(&input.generics)?;
    let container = attrs::container(&input.attrs)?;
    let generic = !input.generics.params.is_empty();
    if let Some(alias) = container.aliases.first().filter(|_| generic) {
        return Err(Error::new_spanned(
            alias,
            "`alias` is not supported on generic types",
        ));
    }
    if let Some(path) = container.data.first().filter(|_| generic) {
        return Err(Error::new_spanned(
            path,
            "type data attributes are not supported on generic types; register the data with `TypeRegistry::add_type_data`",
        ));
    }
    let key = key_expr(&input.ident, &container, &input.generics);
    match &input.data {
        Data::Struct(data) => expand_struct(input, data, &container, &key),
        Data::Enum(data) => expand_enum(input, data, &container, &key),
        Data::Union(_) => Err(Error::new_spanned(
            &input.ident,
            "`Reflect` cannot be derived for unions",
        )),
    }
}

/// The name of an identifier without the `r#` of a raw identifier.
fn plain_name(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_string()
}

fn check_generics(generics: &Generics) -> Result<()> {
    for param in &generics.params {
        if let GenericParam::Lifetime(lifetime) = param {
            return Err(Error::new_spanned(
                lifetime,
                "reflected types cannot have lifetime parameters (reflected values are 'static)",
            ));
        }
    }
    Ok(())
}

/// The generics with a `Typed` bound added for every type parameter.
fn bounded(generics: &Generics) -> Generics {
    let mut generics = generics.clone();
    let idents: Vec<Ident> = generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(param) => Some(param.ident.clone()),
            _ => None,
        })
        .collect();
    let clause = generics.make_where_clause();
    for ident in idents {
        clause
            .predicates
            .push(parse_quote!(#ident: ::reflection::Typed));
    }
    generics
}

/// The expression that makes the `TypeKey`: `module_path!()::Name` or the given `name`, followed by the
/// paths of the type arguments for a generic type.
fn key_expr(ident: &Ident, container: &ContainerAttrs, generics: &Generics) -> TokenStream {
    let base = match &container.name {
        Some(name) => quote!(#name),
        None => {
            let name = plain_name(ident);
            quote!(::core::concat!(::core::module_path!(), "::", #name))
        }
    };
    let args: Vec<TokenStream> = generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(param) => {
                let ident = &param.ident;
                Some(quote! {
                    ::std::string::ToString::to_string(
                        <#ident as ::reflection::Typed>::static_type_info().path()
                    )
                })
            }
            GenericParam::Const(param) => {
                let ident = &param.ident;
                Some(quote!(::std::string::ToString::to_string(&#ident)))
            }
            GenericParam::Lifetime(_) => None,
        })
        .collect();
    if args.is_empty() {
        quote!(::reflection::TypeKey::new(#base))
    } else {
        quote! {
            ::reflection::TypeKey::from_owned(&::std::format!(
                "{}<{}>",
                #base,
                [#(#args),*].join(",")
            ))
        }
    }
}

/// A field that is reflected (not skipped).
struct Field<'a> {
    /// The position among all the fields of the struct or variant, skipped ones included.
    position: usize,
    ident: Option<&'a Ident>,
    ty: &'a Type,
    /// The reflected name; `None` for the fields of tuples.
    name: Option<String>,
    aliases: Vec<String>,
    /// `#[reflect(default)]`: `from_reflect` may use `Default::default()` when the source lacks the field.
    default: bool,
    /// The `.with_metadata(..)` call for the `FieldInfo`, or nothing when the field has no metadata.
    metadata: TokenStream,
}

/// The `.with_metadata(..)` call that gives a description its metadata; empty tokens when there is none, so
/// that types without metadata attributes expand exactly as before.
fn metadata_call(meta: &MetaAttrs, has_default: bool) -> TokenStream {
    let mut calls: Vec<TokenStream> = Vec::new();
    if let Some(docs) = &meta.docs {
        calls.push(quote!(.with_docs(#docs)));
    }
    if let Some(tooltip) = &meta.tooltip {
        calls.push(quote!(.with_tooltip(#tooltip)));
    }
    if let Some(category) = &meta.category {
        calls.push(quote!(.with_category(#category)));
    }
    if let Some((min, max)) = &meta.range {
        calls.push(quote!(.with_range(#min, #max)));
    }
    if meta.readonly {
        calls.push(quote!(.with_readonly()));
    }
    if meta.hidden {
        calls.push(quote!(.with_hidden()));
    }
    if meta.transient {
        calls.push(quote!(.with_transient()));
    }
    if has_default {
        calls.push(quote!(.with_default()));
    }
    for value in &meta.custom {
        calls.push(quote!(.with_custom(#value)));
    }
    if calls.is_empty() {
        return TokenStream::new();
    }
    quote!(.with_metadata(::reflection::Metadata::new() #(#calls)*))
}

impl Field<'_> {
    /// How the field is reached from `self` in a struct: `x` or `0`.
    fn member(&self) -> TokenStream {
        match self.ident {
            Some(ident) => quote!(#ident),
            None => {
                let index = Index::from(self.position);
                quote!(#index)
            }
        }
    }

    /// The name and the aliases, for `match` patterns.
    fn names(&self) -> Vec<String> {
        self.name
            .iter()
            .chain(self.aliases.iter())
            .cloned()
            .collect()
    }

    /// The `FieldInfo` expression for the field at the given position among the reflected fields.
    fn info(&self, index: usize) -> TokenStream {
        let ty = self.ty;
        let aliases = &self.aliases;
        let metadata = &self.metadata;
        match &self.name {
            Some(name) => quote! {
                ::reflection::FieldInfo::named(
                    #name,
                    #index,
                    ::reflection::TypeRef::of::<#ty>(),
                ).with_aliases(&[#(#aliases),*]) #metadata
            },
            None => quote! {
                ::reflection::FieldInfo::unnamed(
                    #index,
                    ::reflection::TypeRef::of::<#ty>(),
                ) #metadata
            },
        }
    }
}

/// The reflected fields of a struct or a variant.
fn collect_fields(fields: &Fields) -> Result<Vec<Field<'_>>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (position, field) in fields.iter().enumerate() {
        let attrs::FieldAttrs {
            skip,
            default,
            rename,
            aliases,
            meta,
        } = attrs::field(&field.attrs)?;
        if skip {
            continue;
        }
        let name = match (&field.ident, rename) {
            (Some(_), Some(rename)) => Some(rename.value()),
            (Some(ident), None) => Some(plain_name(ident)),
            (None, None) => None,
            (None, Some(rename)) => {
                return Err(Error::new_spanned(
                    rename,
                    "`rename` needs a field with a name",
                ));
            }
        };
        if let Some(alias) = aliases.first().filter(|_| field.ident.is_none()) {
            return Err(Error::new_spanned(
                alias,
                "`alias` needs a field with a name",
            ));
        }
        let aliases: Vec<String> = aliases.iter().map(LitStr::value).collect();
        for text in name.iter().chain(aliases.iter()) {
            if !seen.insert(text.clone()) {
                return Err(Error::new_spanned(
                    field,
                    format!("the field name `{text}` is used twice"),
                ));
            }
        }
        out.push(Field {
            position,
            ident: field.ident.as_ref(),
            ty: &field.ty,
            name,
            aliases,
            default,
            metadata: metadata_call(&meta, default),
        });
    }
    Ok(out)
}

/// `match scrutinee { arms, _ => None }`, or just `None` without arms.
fn match_or_none(scrutinee: &TokenStream, arms: &[TokenStream]) -> TokenStream {
    if arms.is_empty() {
        quote!(::core::option::Option::None)
    } else {
        quote! {
            match #scrutinee {
                #(#arms,)*
                _ => ::core::option::Option::None,
            }
        }
    }
}

/// The type of the type data an attribute names: the standard ones by their short names, anything else as
/// written.
fn data_type(path: &Path) -> TokenStream {
    if let Some(ident) = path.get_ident() {
        match ident.to_string().as_str() {
            "Default" => return quote!(::reflection::ReflectDefault),
            "Clone" => return quote!(::reflection::ReflectClone),
            "PartialEq" => return quote!(::reflection::ReflectPartialEq),
            "Debug" => return quote!(::reflection::ReflectDebug),
            "Hash" => return quote!(::reflection::ReflectHash),
            _ => {}
        }
    }
    quote!(#path)
}

/// The three implementations around the kind-specific parts, and the `inventory` entry of a non-generic type.
fn assemble(
    input: &DeriveInput,
    info: &TokenStream,
    reflect_kind: &Ident,
    kind_trait: &TokenStream,
    kind_body: &TokenStream,
    from_reflect: &TokenStream,
    container: &ContainerAttrs,
) -> TokenStream {
    let ident = &input.ident;
    let generics = bounded(&input.generics);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let cell = if input.generics.params.is_empty() {
        quote! {
            static CELL: ::reflection::TypeInfoCell = ::reflection::TypeInfoCell::new();
            CELL.get_or_init(|| #info)
        }
    } else {
        quote! {
            static CELL: ::reflection::GenericTypeInfoCell =
                ::reflection::GenericTypeInfoCell::new();
            CELL.get_or_insert::<Self>(|| #info)
        }
    };
    let data: Vec<TokenStream> = container.data.iter().map(data_type).collect();
    let register_hook = if data.is_empty() {
        quote!()
    } else {
        quote! {
            fn register_type_data(registration: &mut ::reflection::TypeRegistration) {
                #(
                    registration.insert_data(
                        <#data as ::reflection::FromType<Self>>::from_type(),
                    );
                )*
            }
        }
    };
    let submit = if input.generics.params.is_empty() {
        quote! {
            ::reflection::__private::inventory::submit! {
                ::reflection::__private::TypeEntry::new(
                    ::reflection::TypeRegistration::of::<#ident>
                )
            }
        }
    } else {
        quote!()
    };
    quote! {
        #[automatically_derived]
        impl #impl_generics ::reflection::Typed for #ident #ty_generics #where_clause {
            fn static_type_info() -> &'static ::reflection::TypeInfo {
                #cell
            }

            #register_hook

            #from_reflect
        }

        #[automatically_derived]
        impl #impl_generics ::reflection::Reflect for #ident #ty_generics #where_clause {
            ::reflection::impl_reflect_boilerplate!();

            fn reflect_ref(&self) -> ::reflection::ReflectRef<'_> {
                ::reflection::ReflectRef::#reflect_kind(self)
            }

            fn reflect_mut(&mut self) -> ::reflection::ReflectMut<'_> {
                ::reflection::ReflectMut::#reflect_kind(self)
            }
        }

        #[automatically_derived]
        impl #impl_generics #kind_trait for #ident #ty_generics #where_clause {
            #kind_body
        }

        #submit
    }
}

/// `view.field("a").or_else(|| view.field("old"))`: the field called `name` or one of its aliases.
fn lookup_by_name(field: &Field<'_>) -> TokenStream {
    let all = field.names();
    let Some((first, rest)) = all.split_first() else {
        return quote!(::core::option::Option::None);
    };
    let mut expr = quote!(view.field(#first));
    for alias in rest {
        expr = quote!(#expr.or_else(|| view.field(#alias)));
    }
    expr
}

/// The expression that builds a field from the reflected field `lookup` finds: `from_reflect` of its type,
/// or `Default::default()` when the field is `default` and the source lacks it.
fn field_value(field: &Field<'_>, lookup: &TokenStream) -> TokenStream {
    let ty = field.ty;
    if field.default {
        quote! {
            match #lookup {
                ::core::option::Option::Some(found) => {
                    <#ty as ::reflection::Typed>::from_reflect(found)?
                }
                ::core::option::Option::None => ::core::default::Default::default(),
            }
        }
    } else {
        quote!(<#ty as ::reflection::Typed>::from_reflect(#lookup?)?)
    }
}

/// The expression that builds `path` (a struct or a variant) with the given declared `fields`: reflected ones
/// from `lookup(reflected index, field)`, skipped ones with `Default::default()`.
fn construct(
    path: &TokenStream,
    fields: &Fields,
    reflected: &[Field<'_>],
    lookup: impl Fn(usize, &Field<'_>) -> TokenStream,
) -> TokenStream {
    let values: Vec<TokenStream> = (0..fields.len())
        .map(|position| {
            match reflected
                .iter()
                .enumerate()
                .find(|(_, field)| field.position == position)
            {
                Some((index, field)) => field_value(field, &lookup(index, field)),
                None => quote!(::core::default::Default::default()),
            }
        })
        .collect();
    match fields {
        Fields::Named(_) => {
            let idents: Vec<&Ident> = fields
                .iter()
                .filter_map(|field| field.ident.as_ref())
                .collect();
            quote!(#path { #(#idents: #values),* })
        }
        Fields::Unnamed(_) => quote!(#path(#(#values),*)),
        Fields::Unit => quote!(#path),
    }
}

/// `Typed::from_reflect` for a struct or a tuple struct.
fn struct_from_reflect(fields: &Fields, reflected: &[Field<'_>]) -> TokenStream {
    let tuple = matches!(fields, Fields::Unnamed(_));
    let value = construct(&quote!(Self), fields, reflected, |index, field| {
        if tuple {
            quote!(view.field(#index))
        } else {
            lookup_by_name(field)
        }
    });
    let kind = if tuple {
        quote!(TupleStruct)
    } else {
        quote!(Struct)
    };
    quote! {
        fn from_reflect(value: &dyn ::reflection::Reflect) -> ::core::option::Option<Self> {
            let ::reflection::ReflectRef::#kind(view) =
                ::reflection::Reflect::reflect_ref(value)
            else {
                return ::core::option::Option::None;
            };
            let _ = view;
            ::core::option::Option::Some(#value)
        }
    }
}

fn expand_struct(
    input: &DeriveInput,
    data: &DataStruct,
    container: &ContainerAttrs,
    key: &TokenStream,
) -> Result<TokenStream> {
    let fields = collect_fields(&data.fields)?;
    let type_aliases: Vec<String> = container.aliases.iter().map(LitStr::value).collect();
    let type_meta = metadata_call(&container.meta, false);
    let field_infos: Vec<TokenStream> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| field.info(index))
        .collect();
    let count = fields.len();
    let some = quote!(::core::option::Option::Some);
    let reflect = quote!(::reflection::Reflect);

    if matches!(data.fields, Fields::Unnamed(_)) {
        let info = quote! {
            {
                let key = #key;
                ::reflection::TypeInfo::TupleStruct(
                    ::reflection::TupleStructInfo::new::<Self>(
                        key,
                        ::std::vec![#(#field_infos),*],
                    )
                    .with_aliases(&[#(#type_aliases),*]) #type_meta,
                )
            }
        };
        let index_ident = quote!(index);
        let shared: Vec<TokenStream> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let member = f.member();
                quote!(#i => #some(&self.#member as &dyn #reflect))
            })
            .collect();
        let exclusive: Vec<TokenStream> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let member = f.member();
                quote!(#i => #some(&mut self.#member as &mut dyn #reflect))
            })
            .collect();
        let shared = match_or_none(&index_ident, &shared);
        let exclusive = match_or_none(&index_ident, &exclusive);
        let body = quote! {
            fn field_len(&self) -> usize {
                #count
            }

            fn field(&self, index: usize) -> ::core::option::Option<&dyn #reflect> {
                let _ = index;
                #shared
            }

            fn field_mut(
                &mut self,
                index: usize,
            ) -> ::core::option::Option<&mut dyn #reflect> {
                let _ = index;
                #exclusive
            }
        };
        let from_reflect = if container.no_from_reflect {
            quote!()
        } else {
            struct_from_reflect(&data.fields, &fields)
        };
        let kind = format_ident!("TupleStruct");
        return Ok(assemble(
            input,
            &info,
            &kind,
            &quote!(::reflection::TupleStruct),
            &body,
            &from_reflect,
            container,
        ));
    }

    let info = quote! {
        {
            let key = #key;
            ::reflection::TypeInfo::Struct(
                ::reflection::StructInfo::new::<Self>(
                    key,
                    ::std::vec![#(#field_infos),*],
                )
                .with_aliases(&[#(#type_aliases),*]) #type_meta,
            )
        }
    };
    let index_ident = quote!(index);
    let name_ident = quote!(name);
    let names_at: Vec<TokenStream> = fields
        .iter()
        .enumerate()
        .filter_map(|(i, f)| f.name.as_ref().map(|name| quote!(#i => #some(#name))))
        .collect();
    let by_name = |exclusive: bool| -> Vec<TokenStream> {
        fields
            .iter()
            .map(|f| {
                let member = f.member();
                let names = f.names();
                if exclusive {
                    quote!(#(#names)|* => #some(&mut self.#member as &mut dyn #reflect))
                } else {
                    quote!(#(#names)|* => #some(&self.#member as &dyn #reflect))
                }
            })
            .collect()
    };
    let by_index = |exclusive: bool| -> Vec<TokenStream> {
        fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let member = f.member();
                if exclusive {
                    quote!(#i => #some(&mut self.#member as &mut dyn #reflect))
                } else {
                    quote!(#i => #some(&self.#member as &dyn #reflect))
                }
            })
            .collect()
    };
    let name_at = match_or_none(&index_ident, &names_at);
    let field = match_or_none(&name_ident, &by_name(false));
    let field_mut = match_or_none(&name_ident, &by_name(true));
    let field_at = match_or_none(&index_ident, &by_index(false));
    let field_at_mut = match_or_none(&index_ident, &by_index(true));
    let body = quote! {
        fn field_len(&self) -> usize {
            #count
        }

        fn name_at(&self, index: usize) -> ::core::option::Option<&'static str> {
            let _ = index;
            #name_at
        }

        fn field(&self, name: &str) -> ::core::option::Option<&dyn #reflect> {
            let _ = name;
            #field
        }

        fn field_mut(&mut self, name: &str) -> ::core::option::Option<&mut dyn #reflect> {
            let _ = name;
            #field_mut
        }

        fn field_at(&self, index: usize) -> ::core::option::Option<&dyn #reflect> {
            let _ = index;
            #field_at
        }

        fn field_at_mut(
            &mut self,
            index: usize,
        ) -> ::core::option::Option<&mut dyn #reflect> {
            let _ = index;
            #field_at_mut
        }
    };
    let from_reflect = if container.no_from_reflect {
        quote!()
    } else {
        struct_from_reflect(&data.fields, &fields)
    };
    let kind = format_ident!("Struct");
    Ok(assemble(
        input,
        &info,
        &kind,
        &quote!(::reflection::Struct),
        &body,
        &from_reflect,
        container,
    ))
}

/// The kind of fields a variant has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unit,
    Tuple,
    Named,
}

struct Variant<'a> {
    ident: &'a Ident,
    index: usize,
    name: String,
    aliases: Vec<String>,
    kind: Kind,
    /// The number of fields, skipped ones included.
    total: usize,
    /// The declared fields, skipped ones included.
    syn_fields: &'a Fields,
    fields: Vec<Field<'a>>,
    /// The `.with_metadata(..)` call for the `VariantInfo`, or nothing.
    metadata: TokenStream,
}

/// The name of the binding of the reflected field at the given position.
fn binding(index: usize) -> Ident {
    format_ident!("__field_{}", index)
}

impl Variant<'_> {
    /// A pattern that matches the variant and ignores its fields.
    fn pat_all(&self) -> TokenStream {
        let ident = self.ident;
        match self.kind {
            Kind::Unit => quote!(Self::#ident),
            Kind::Tuple => quote!(Self::#ident(..)),
            Kind::Named => quote!(Self::#ident { .. }),
        }
    }

    /// A pattern that matches the variant and binds the reflected fields to `__field_0`, `__field_1`, ...
    fn pat_bind(&self) -> TokenStream {
        let ident = self.ident;
        match self.kind {
            Kind::Unit => quote!(Self::#ident),
            Kind::Tuple => {
                let mut slots = vec![quote!(_); self.total];
                for (index, field) in self.fields.iter().enumerate() {
                    let bind = binding(index);
                    slots[field.position] = quote!(#bind);
                }
                quote!(Self::#ident(#(#slots),*))
            }
            Kind::Named => {
                let pairs: Vec<TokenStream> = self
                    .fields
                    .iter()
                    .enumerate()
                    .filter_map(|(index, field)| {
                        field.ident.map(|name| {
                            let bind = binding(index);
                            quote!(#name: #bind)
                        })
                    })
                    .collect();
                quote!(Self::#ident { #(#pairs,)* .. })
            }
        }
    }

    fn info(&self) -> TokenStream {
        let name = &self.name;
        let index = self.index;
        let aliases = &self.aliases;
        let metadata = &self.metadata;
        let fields: Vec<TokenStream> = self
            .fields
            .iter()
            .enumerate()
            .map(|(position, field)| field.info(position))
            .collect();
        match self.kind {
            Kind::Unit => quote! {
                ::reflection::VariantInfo::unit(#name, #index)
                    .with_aliases(&[#(#aliases),*]) #metadata
            },
            Kind::Tuple => quote! {
                ::reflection::VariantInfo::tuple(#name, #index, ::std::vec![#(#fields),*])
                    .with_aliases(&[#(#aliases),*]) #metadata
            },
            Kind::Named => quote! {
                ::reflection::VariantInfo::with_named_fields(
                    #name,
                    #index,
                    ::std::vec![#(#fields),*],
                )
                .with_aliases(&[#(#aliases),*]) #metadata
            },
        }
    }

    fn kind_path(&self) -> TokenStream {
        match self.kind {
            Kind::Unit => quote!(::reflection::VariantKind::Unit),
            Kind::Tuple => quote!(::reflection::VariantKind::Tuple),
            Kind::Named => quote!(::reflection::VariantKind::Struct),
        }
    }
}

fn collect_variants(data: &DataEnum) -> Result<Vec<Variant<'_>>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (index, variant) in data.variants.iter().enumerate() {
        let attrs::VariantAttrs {
            rename,
            aliases,
            meta,
        } = attrs::variant(&variant.attrs)?;
        let name = rename
            .as_ref()
            .map_or_else(|| plain_name(&variant.ident), LitStr::value);
        let aliases: Vec<String> = aliases.iter().map(LitStr::value).collect();
        for text in std::iter::once(&name).chain(aliases.iter()) {
            if !seen.insert(text.clone()) {
                return Err(Error::new_spanned(
                    variant,
                    format!("the variant name `{text}` is used twice"),
                ));
            }
        }
        let kind = match &variant.fields {
            Fields::Unit => Kind::Unit,
            Fields::Unnamed(_) => Kind::Tuple,
            Fields::Named(_) => Kind::Named,
        };
        out.push(Variant {
            ident: &variant.ident,
            index,
            name,
            aliases,
            kind,
            total: variant.fields.len(),
            syn_fields: &variant.fields,
            fields: collect_fields(&variant.fields)?,
            metadata: metadata_call(&meta, false),
        });
    }
    Ok(out)
}

/// One `match self` arm per variant: with the bindings if `body` gives an expression for the variant, with
/// the fields ignored and `None` otherwise.
fn arms_for(
    variants: &[Variant<'_>],
    body: impl Fn(&Variant<'_>) -> Option<TokenStream>,
) -> Vec<TokenStream> {
    variants
        .iter()
        .map(|variant| match body(variant) {
            Some(body) => {
                let pat = variant.pat_bind();
                quote!(#pat => #body)
            }
            None => {
                let pat = variant.pat_all();
                quote!(#pat => ::core::option::Option::None)
            }
        })
        .collect()
}

/// `Typed::from_reflect` for an enum: pick the variant by name or alias, then build its fields.
fn enum_from_reflect(variants: &[Variant<'_>]) -> TokenStream {
    let arms: Vec<TokenStream> = variants
        .iter()
        .map(|variant| {
            let ident = variant.ident;
            let tuple = variant.kind == Kind::Tuple;
            let value = construct(
                &quote!(Self::#ident),
                variant.syn_fields,
                &variant.fields,
                |index, field| {
                    if tuple {
                        quote!(view.field_at(#index))
                    } else {
                        lookup_by_name(field)
                    }
                },
            );
            let names: Vec<&String> = std::iter::once(&variant.name)
                .chain(variant.aliases.iter())
                .collect();
            quote!(#(#names)|* => ::core::option::Option::Some(#value))
        })
        .collect();
    quote! {
        fn from_reflect(value: &dyn ::reflection::Reflect) -> ::core::option::Option<Self> {
            let ::reflection::ReflectRef::Enum(view) = ::reflection::Reflect::reflect_ref(value)
            else {
                return ::core::option::Option::None;
            };
            match view.variant_name() {
                #(#arms,)*
                _ => ::core::option::Option::None,
            }
        }
    }
}

fn expand_enum(
    input: &DeriveInput,
    data: &DataEnum,
    container: &ContainerAttrs,
    key: &TokenStream,
) -> Result<TokenStream> {
    if data.variants.is_empty() {
        return Err(Error::new_spanned(
            &input.ident,
            "`Reflect` cannot be derived for an enum without variants",
        ));
    }
    let variants = collect_variants(data)?;
    let type_aliases: Vec<String> = container.aliases.iter().map(LitStr::value).collect();
    let type_meta = metadata_call(&container.meta, false);
    let variant_infos: Vec<TokenStream> = variants.iter().map(Variant::info).collect();
    let info = quote! {
        {
            let key = #key;
            ::reflection::TypeInfo::Enum(
                ::reflection::EnumInfo::new::<Self>(
                    key,
                    ::std::vec![#(#variant_infos),*],
                )
                .with_aliases(&[#(#type_aliases),*]) #type_meta,
            )
        }
    };

    let some = quote!(::core::option::Option::Some);
    let reflect = quote!(::reflection::Reflect);
    let index_ident = quote!(index);
    let name_ident = quote!(name);

    let names: Vec<TokenStream> = variants
        .iter()
        .map(|v| {
            let pat = v.pat_all();
            let name = &v.name;
            quote!(#pat => #name)
        })
        .collect();
    let indices: Vec<TokenStream> = variants
        .iter()
        .map(|v| {
            let pat = v.pat_all();
            let index = v.index;
            quote!(#pat => #index)
        })
        .collect();
    let kinds: Vec<TokenStream> = variants
        .iter()
        .map(|v| {
            let pat = v.pat_all();
            let kind = v.kind_path();
            quote!(#pat => #kind)
        })
        .collect();
    let lens: Vec<TokenStream> = variants
        .iter()
        .map(|v| {
            let pat = v.pat_all();
            let len = v.fields.len();
            quote!(#pat => #len)
        })
        .collect();
    let names_at = arms_for(&variants, |_| None::<TokenStream>);
    // `name_at` needs no bindings: build its arms by hand.
    let names_at: Vec<TokenStream> = variants
        .iter()
        .zip(names_at)
        .map(|(v, fallback)| {
            if v.kind != Kind::Named || v.fields.is_empty() {
                return fallback;
            }
            let pat = v.pat_all();
            let arms: Vec<TokenStream> = v
                .fields
                .iter()
                .enumerate()
                .filter_map(|(i, f)| f.name.as_ref().map(|name| quote!(#i => #some(#name))))
                .collect();
            let body = match_or_none(&index_ident, &arms);
            quote!(#pat => #body)
        })
        .collect();
    let by_index = |exclusive: bool| {
        arms_for(&variants, |v| {
            if v.fields.is_empty() {
                return None;
            }
            let arms: Vec<TokenStream> = (0..v.fields.len())
                .map(|i| {
                    let bind = binding(i);
                    if exclusive {
                        quote!(#i => #some(#bind as &mut dyn #reflect))
                    } else {
                        quote!(#i => #some(#bind as &dyn #reflect))
                    }
                })
                .collect();
            Some(match_or_none(&index_ident, &arms))
        })
    };
    let by_name = |exclusive: bool| {
        arms_for(&variants, |v| {
            if v.kind != Kind::Named || v.fields.is_empty() {
                return None;
            }
            let arms: Vec<TokenStream> = v
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let bind = binding(i);
                    let names = f.names();
                    if exclusive {
                        quote!(#(#names)|* => #some(#bind as &mut dyn #reflect))
                    } else {
                        quote!(#(#names)|* => #some(#bind as &dyn #reflect))
                    }
                })
                .collect();
            Some(match_or_none(&name_ident, &arms))
        })
    };
    let field = by_name(false);
    let field_mut = by_name(true);
    let field_at = by_index(false);
    let field_at_mut = by_index(true);

    let body = quote! {
        fn variant_name(&self) -> &'static str {
            match self {
                #(#names,)*
            }
        }

        fn variant_index(&self) -> usize {
            match self {
                #(#indices,)*
            }
        }

        fn variant_kind(&self) -> ::reflection::VariantKind {
            match self {
                #(#kinds,)*
            }
        }

        fn field_len(&self) -> usize {
            match self {
                #(#lens,)*
            }
        }

        fn name_at(&self, index: usize) -> ::core::option::Option<&'static str> {
            let _ = index;
            match self {
                #(#names_at,)*
            }
        }

        fn field(&self, name: &str) -> ::core::option::Option<&dyn #reflect> {
            let _ = name;
            match self {
                #(#field,)*
            }
        }

        fn field_mut(&mut self, name: &str) -> ::core::option::Option<&mut dyn #reflect> {
            let _ = name;
            match self {
                #(#field_mut,)*
            }
        }

        fn field_at(&self, index: usize) -> ::core::option::Option<&dyn #reflect> {
            let _ = index;
            match self {
                #(#field_at,)*
            }
        }

        fn field_at_mut(
            &mut self,
            index: usize,
        ) -> ::core::option::Option<&mut dyn #reflect> {
            let _ = index;
            match self {
                #(#field_at_mut,)*
            }
        }
    };
    let from_reflect = if container.no_from_reflect {
        quote!()
    } else {
        enum_from_reflect(&variants)
    };
    let kind = format_ident!("Enum");
    Ok(assemble(
        input,
        &info,
        &kind,
        &quote!(::reflection::Enum),
        &body,
        &from_reflect,
        container,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expands the derive for `source` and checks that the result is a valid sequence of items.
    fn expand_source(source: &str) -> Result<(usize, String)> {
        let input: DeriveInput = syn::parse_str(source)?;
        let tokens = derive(&input)?;
        let file: syn::File = syn::parse2(tokens.clone())?;
        Ok((file.items.len(), tokens.to_string()))
    }

    fn error_of(source: &str) -> String {
        match expand_source(source) {
            Ok(_) => panic!("the expansion should have failed"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn struct_expands_to_three_impls_and_an_inventory_entry() {
        let (items, text) = expand_source("struct Player { name: String, health: u32 }").unwrap();
        assert_eq!(items, 4);
        assert!(text.contains("submit"));
        assert!(text.contains("TypeEntry"));
        assert!(text.contains("StructInfo"));
        assert!(text.contains("\"health\""));
    }

    #[test]
    fn tuple_unit_and_enum_expand() {
        assert_eq!(expand_source("struct Meters(f32);").unwrap().0, 4);
        assert!(
            expand_source("struct Meters(f32);")
                .unwrap()
                .1
                .contains("TupleStructInfo")
        );
        assert_eq!(expand_source("struct Marker;").unwrap().0, 4);
        let (items, text) =
            expand_source("enum Shape { Empty, Circle(f32), Rect { w: f32, h: f32 } }").unwrap();
        assert_eq!(items, 4);
        assert!(text.contains("EnumInfo"));
        assert!(text.contains("with_named_fields"));
    }

    #[test]
    fn generic_types_get_no_inventory_entry() {
        let (items, text) = expand_source("struct Pair<A, B> { a: A, b: B }").unwrap();
        assert_eq!(items, 3);
        assert!(!text.contains("submit"));
    }

    #[test]
    fn type_data_attributes_write_the_hook() {
        let (_, text) =
            expand_source("#[reflect(Default, Clone, PartialEq, Debug, Hash)] struct S { a: u8 }")
                .unwrap();
        assert!(text.contains("register_type_data"));
        for name in [
            "ReflectDefault",
            "ReflectClone",
            "ReflectPartialEq",
            "ReflectDebug",
            "ReflectHash",
        ] {
            assert!(text.contains(name), "{name} is missing");
        }
        let (_, text) = expand_source("#[reflect(my::ReflectThing)] struct S;").unwrap();
        assert!(text.contains("ReflectThing"));
        assert!(text.contains("FromType"));
        let (_, text) = expand_source("struct S { a: u8 }").unwrap();
        assert!(!text.contains("register_type_data"));
    }

    #[test]
    fn from_reflect_is_written_unless_opted_out() {
        let (_, text) = expand_source("struct S { a: u8 }").unwrap();
        assert!(text.contains("from_reflect"));
        let (_, text) = expand_source("struct S(u8, u16);").unwrap();
        assert!(text.contains("from_reflect"));
        let (_, text) = expand_source("enum E { A, B(u8), C { x: u8 } }").unwrap();
        assert!(text.contains("from_reflect"));
        assert!(text.contains("variant_name"));
        let (_, text) = expand_source("#[reflect(no_from_reflect)] struct S { a: u8 }").unwrap();
        assert!(!text.contains("from_reflect"));
    }

    #[test]
    fn default_and_skipped_fields_use_default() {
        let (_, text) =
            expand_source("struct S { #[reflect(default)] a: u8, #[reflect(skip)] b: u8, c: u8 }")
                .unwrap();
        assert!(text.contains("Default"));
        let (_, text) = expand_source("struct S(u8, #[reflect(skip)] u8);").unwrap();
        assert!(text.contains("Default"));
        let (_, text) = expand_source("struct S { a: u8 }").unwrap();
        assert!(!text.contains("Default"));
    }

    #[test]
    fn generics_get_typed_bounds() {
        let (_, text) = expand_source("struct Pair<A, B> { a: A, b: B }").unwrap();
        assert!(text.contains("GenericTypeInfoCell"));
        assert!(text.contains("where"));
        let (_, text) = expand_source("struct Buf<const N: usize> { data: [u8; N] }").unwrap();
        assert!(text.contains("to_string"));
        let (_, text) = expand_source("struct Plain { x: u8 }").unwrap();
        assert!(text.contains("TypeInfoCell"));
        assert!(!text.contains("GenericTypeInfoCell"));
    }

    #[test]
    fn attributes_are_used() {
        let (_, text) = expand_source(
            "#[reflect(name = \"game::Hero\", alias = \"game::OldHero\")] struct Hero { \
             #[reflect(rename = \"hp\", alias = \"health\")] life: u32, \
             #[reflect(skip)] cache: u8 }",
        )
        .unwrap();
        assert!(text.contains("\"game::Hero\""));
        assert!(text.contains("\"game::OldHero\""));
        assert!(text.contains("\"hp\""));
        assert!(text.contains("\"health\""));
        assert!(!text.contains("\"cache\""));
        assert!(!text.contains("\"life\""));
    }

    #[test]
    fn raw_identifiers_lose_the_prefix() {
        let (_, text) = expand_source("struct Event { r#type: u8 }").unwrap();
        assert!(text.contains("\"type\""));
        assert!(!text.contains("\"r#type\""));
    }

    #[test]
    fn rejects_unsupported_input() {
        assert!(error_of("union U { a: u8 }").contains("unions"));
        assert!(error_of("struct S<'a> { a: &'a u8 }").contains("lifetime"));
        assert!(error_of("enum E {}").contains("without variants"));
        assert!(error_of("#[reflect(frobnicate)] struct S { a: u8 }").contains("unsupported"));
        assert!(error_of("struct S { #[reflect(frobnicate)] a: u8 }").contains("unsupported"));
        assert!(error_of("enum E { #[reflect(skip)] A }").contains("unsupported"));
        assert!(error_of("#[reflect(alias = \"x\")] struct S<T> { a: T }").contains("generic"));
        assert!(error_of("#[reflect(Default)] struct S<T> { a: T }").contains("generic"));
        assert!(error_of("struct S { #[reflect(default = 1)] a: u8 }").contains("expected"));
        assert!(
            error_of("#[reflect(no_from_reflect = 1)] struct S { a: u8 }").contains("expected")
        );
        assert!(error_of("#[reflect(Default = 1)] struct S { a: u8 }").contains("expected"));
        assert!(error_of("struct S(#[reflect(rename = \"x\")] u8);").contains("name"));
        assert!(error_of("#[reflect(name = \"\")] struct S { a: u8 }").contains("empty"));
    }

    #[test]
    fn rejects_duplicate_names() {
        assert!(error_of("struct S { a: u8, #[reflect(rename = \"a\")] b: u8 }").contains("twice"));
        assert!(error_of("struct S { a: u8, #[reflect(alias = \"a\")] b: u8 }").contains("twice"));
        assert!(error_of("enum E { A, #[reflect(rename = \"A\")] B }").contains("twice"));
    }

    #[test]
    fn metadata_attributes_are_expanded() {
        let (_, text) = expand_source(
            "#[reflect(category = \"Gameplay\", transient, attr = Marker { id: 1 })] \
             struct Hero { \
             #[reflect(tooltip = \"Hit points\", range(min = 0.0, max = 100.0), readonly, hidden)] hp: f32, \
             #[reflect(default)] mana: u32 }",
        )
        .unwrap();
        assert!(text.contains("with_metadata"));
        assert!(text.contains("with_category"));
        assert!(text.contains("with_transient"));
        assert!(text.contains("with_custom"));
        assert!(text.contains("with_tooltip"));
        assert!(text.contains("with_range"));
        assert!(text.contains("with_readonly"));
        assert!(text.contains("with_hidden"));
        assert!(text.contains("with_default"));
    }

    #[test]
    fn variants_take_their_own_metadata() {
        let (_, text) = expand_source(
            "enum E { #[reflect(tooltip = \"Idle\", category = \"State\", hidden, attr = 5u8)] A, B(u8) }",
        )
        .unwrap();
        assert!(text.contains("with_tooltip"));
        assert!(text.contains("with_category"));
        assert!(text.contains("with_custom"));
    }

    #[test]
    fn no_metadata_means_no_metadata_call() {
        let (_, text) = expand_source("struct S { a: u8 }").unwrap();
        assert!(!text.contains("with_metadata"));
        let (_, text) = expand_source("enum E { A, B }").unwrap();
        assert!(!text.contains("with_metadata"));
    }

    #[test]
    fn metadata_attributes_are_checked() {
        // `range` only on fields
        assert!(error_of("#[reflect(range(min = 0, max = 1))] struct S { a: u8 }").contains("unsupported"));
        assert!(error_of("enum E { #[reflect(range(min = 0, max = 1))] A }").contains("unsupported"));
        // `readonly` and `transient` not on variants
        assert!(error_of("enum E { #[reflect(readonly)] A }").contains("unsupported"));
        assert!(error_of("enum E { #[reflect(transient)] A }").contains("unsupported"));
        // given twice
        assert!(error_of("struct S { #[reflect(tooltip = \"a\", tooltip = \"b\")] a: u8 }").contains("twice"));
        assert!(error_of("struct S { #[reflect(category = \"a\", category = \"b\")] a: u8 }").contains("twice"));
        // incomplete or unknown range
        assert!(error_of("struct S { #[reflect(range(min = 0))] a: u8 }").contains("max"));
        assert!(error_of("struct S { #[reflect(range(low = 0, high = 1))] a: u8 }").contains("min"));
        // empty text
        assert!(error_of("struct S { #[reflect(tooltip = \"\")] a: u8 }").contains("empty"));
        // `hidden = true` is not a flag with a value
        assert!(expand_source("struct S { #[reflect(hidden = true)] a: u8 }").is_err());
    }

    #[cfg(feature = "docs")]
    #[test]
    fn doc_comments_are_captured_with_the_docs_feature() {
        let (_, text) = expand_source("/// The hero.\n/// Second line.\nstruct Hero { /** Life. */ hp: u32 }").unwrap();
        assert!(text.contains("with_docs"));
        assert!(text.contains("The hero.\\nSecond line."));
    }

    #[cfg(not(feature = "docs"))]
    #[test]
    fn doc_comments_are_ignored_without_the_docs_feature() {
        let (_, text) = expand_source("/// The hero.\nstruct Hero { /// Life.\n hp: u32 }").unwrap();
        assert!(!text.contains("with_docs"));
    }
}
