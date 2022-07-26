#[macro_use]
mod utils;
mod attr;

use attr::{ContainerAttr, EnumAttr, FieldAttr, StructAttr, Tagged, VariantAttr};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parse_macro_input, parse_quote, ConstParam, Data, DataEnum, DataStruct, DeriveInput, Field,
    Fields, GenericParam, Generics, Ident, LifetimeDef, TypeParam, WhereClause,
};

#[proc_macro_derive(Type, attributes(specta, serde))]
pub fn derive_type(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    derive_type_internal(input, "specta".into())
}

/// This macro is exposed from rspc as a wrapper around [Type] with a correct import path.
/// This is exposed from here so rspc doesn't need a macro package for 4 lines of code.
#[doc(hidden)]
#[proc_macro_derive(RSPCType, attributes(specta, serde))]
pub fn derive_rspc_type(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    derive_type_internal(input, "rspc::internal::specta".into())
}

fn derive_type_internal(
    input: proc_macro::TokenStream,
    default_crate_name: String,
) -> proc_macro::TokenStream {
    let derive_input = parse_macro_input!(input);

    let DeriveInput {
        ident,
        generics,
        data,
        attrs,
        ..
    } = &derive_input;

    let container_attrs = ContainerAttr::from_attrs(attrs).unwrap();

    let crate_name: TokenStream = container_attrs
        .crate_name
        .clone()
        .unwrap_or_else(|| default_crate_name)
        .parse()
        .unwrap();
    let crate_ref = quote!(::#crate_name);

    let name_str = container_attrs.rename.clone().unwrap_or(ident.to_string());

    let name = match container_attrs.inline {
        true => quote!(None),
        false => quote!(Some(#name_str.to_string())),
    };

    let ty = match data {
        Data::Struct(data) => {
            let struct_attrs = StructAttr::from_attrs(attrs).unwrap();

            parse_struct(
                &name_str,
                &ident,
                &struct_attrs,
                &container_attrs,
                &generics,
                &crate_ref,
                data,
            )
        }
        Data::Enum(data) => {
            let enum_attrs = EnumAttr::from_attrs(attrs).unwrap();

            parse_enum(
                &name_str,
                &ident,
                &enum_attrs,
                &container_attrs,
                &generics,
                &crate_ref,
                data,
            )
        }
        Data::Union(_) => panic!("Type 'Union' is not supported by specta!"),
    };

    let impl_start = generate_impl(&crate_name, &ident, &generics);
    quote! {
       #impl_start {
            fn def(mut defs: &mut #crate_ref::TypeDefs) -> #crate_ref::DataType {
                #ty
            }

            fn base(defs: &mut #crate_ref::TypeDefs) -> #crate_ref::DataType {
                Self::def(defs)
            }

            fn name() -> Option<String> {
                #name
            }

            fn refr() -> #crate_ref::DataType {
                #crate_ref::DataType::Reference(#name_str.to_string())
            }
        }
    }
    .into()
}

fn type_ident_with_generics(ident: &Ident, generics: &Generics) -> TokenStream {
    let generics = generics
        .params
        .iter()
        .map(|param| match param {
            GenericParam::Type(param) => {
                let ident = &param.ident;
                quote! { #ident }
            }
            GenericParam::Lifetime(param) => {
                let ident = &param.lifetime;
                quote! { #ident }
            }
            GenericParam::Const(_) => panic!("const generics are not supported by specta!"), // TODO: Support const generics
        })
        .collect::<Vec<_>>();

    quote! { #ident<#(#generics),*> }
}

fn parse_struct(
    struct_name_str: &str,
    struct_ident: &Ident,
    _struct_attrs: &StructAttr,
    container_attrs: &ContainerAttr,
    generics: &Generics,
    crate_ref: &TokenStream,
    data: &DataStruct,
) -> TokenStream {
    let inline = container_attrs.inline;

    let fields = data
        .fields
        .iter()
        .map(|field| {
            let attrs = FieldAttr::from_attrs(&field.attrs).unwrap();

            (field, attrs)
        })
        .filter(|(_, attrs)| !attrs.skip);

    let struct_ident = type_ident_with_generics(&struct_ident, &generics);
    match &data.fields {
        Fields::Unit => quote!(#crate_ref::DataType::Tuple(#crate_ref::TupleType {
            name: #struct_name_str.to_string(),
            id: std::any::TypeId::of::<#struct_ident>(),
            fields: vec![],
        })),
        Fields::Unnamed(_) => {
            let fields = fields
                .clone()
                .map(|(field, field_attrs)| {
                    parse_tuple_struct_field(&field, &container_attrs, &field_attrs, crate_ref)
                })
                .collect::<Vec<_>>();

            quote!(#crate_ref::DataType::Tuple(#crate_ref::TupleType {
                name: #struct_name_str.to_string(),
                id: std::any::TypeId::of::<#struct_ident>(),
                fields: (vec![#(#fields),*] as Vec<Vec<_>>)
                    .into_iter()
                    .flatten()
                    .collect::<Vec<#crate_ref::DataType>>(),
            }))
        }
        Fields::Named(_) => {
            let fields = fields
                .clone()
                .map(|(field, field_attrs)| {
                    parse_named_struct_field(field, &container_attrs, &field_attrs, crate_ref)
                })
                .collect::<Vec<_>>();

            let tag = container_attrs
                .tag
                .as_ref()
                .map(|t| quote!(Some(#t.to_string())))
                .unwrap_or(quote!(None));

            let generics = parse_generics(crate_ref, generics);
            quote! {{
                if #inline == false {
                    let generics = #generics;
                    defs.insert(
                        #struct_name_str.to_string(),
                        #crate_ref::DataType::Object(#crate_ref::ObjectType {
                            name: #struct_name_str.to_string(),
                            inline: #inline,
                            id: std::any::TypeId::of::<#struct_ident>(),
                            generics,
                            fields: vec![],
                            tag: #tag
                        })
                    );
                }

                let new_fields = (vec![#(#fields),*] as Vec<Vec<_>>)
                    .into_iter()
                    .flatten()
                    .collect::<Vec<#crate_ref::ObjectField>>();

                if #inline == false {
                    match defs.get_mut(&#struct_name_str.to_string()).unwrap() {
                        #crate_ref::DataType::Object(#crate_ref::ObjectType {
                            fields,
                            ..
                        }) => {
                            fields.extend(new_fields);
                        }
                        _ => panic!("Unexpected data type for struct"),
                    }

                    defs.get(&#struct_name_str.to_string()).unwrap().clone()
                } else {
                    #crate_ref::DataType::Object(#crate_ref::ObjectType {
                        name: #struct_name_str.to_string(),
                        inline: #inline,
                        id: std::any::TypeId::of::<#struct_ident>(),
                        generics: #generics,
                        fields: new_fields,
                        tag: #tag
                    })
                }
            }}
        }
    }
}

fn parse_generics(crate_ref: &TokenStream, generics: &Generics) -> TokenStream {
    let generics = generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(ty) => {
                let ident = ty.ident.clone();
                Some(quote! {
                    #crate_ref::Generic::TypeParam {
                        name: stringify!(#ident).into(),
                        ty: <#ident as #crate_ref::Type>::def(defs),
                    }
                })
            }
            GenericParam::Lifetime(_) => None,
            GenericParam::Const(_) => None, // TODO: Support const generics
        })
        .collect::<Vec<_>>();

    quote! { vec![#(#generics),*] }
}

// Code copied from ts-rs. Thanks to it's original author!
// generate start of the `impl TS for #ty` block, up to (excluding) the open brace
fn generate_impl(crate_name: &TokenStream, ty: &Ident, generics: &Generics) -> TokenStream {
    use GenericParam::*;

    let bounds = generics.params.iter().map(|param| match param {
        Type(TypeParam {
            ident,
            colon_token,
            bounds,
            ..
        }) => quote!(#ident #colon_token #bounds),
        Lifetime(LifetimeDef {
            lifetime,
            colon_token,
            bounds,
            ..
        }) => quote!(#lifetime #colon_token #bounds),
        Const(ConstParam {
            const_token,
            ident,
            colon_token,
            ty,
            ..
        }) => quote!(#const_token #ident #colon_token #ty),
    });
    let type_args = generics.params.iter().map(|param| match param {
        Type(TypeParam { ident, .. }) | Const(ConstParam { ident, .. }) => quote!(#ident),
        Lifetime(LifetimeDef { lifetime, .. }) => quote!(#lifetime),
    });

    let where_bound = add_ts_to_where_clause(crate_name, generics);
    quote!(impl <#(#bounds),*> #crate_name::Type for #ty <#(#type_args),*> #where_bound)
}

// Code copied from ts-rs. Thanks to it's original author!
fn add_ts_to_where_clause(crate_name: &TokenStream, generics: &Generics) -> Option<WhereClause> {
    let generic_types = generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            GenericParam::Type(ty) => Some(ty.ident.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if generic_types.is_empty() {
        return generics.where_clause.clone();
    }
    match generics.where_clause {
        None => Some(parse_quote! { where #( #generic_types : #crate_name::Type + 'static ),* }),
        Some(ref w) => {
            let bounds = w.predicates.iter();
            Some(
                parse_quote! { where #(#bounds,)* #( #generic_types : #crate_name::Type + 'static ),* },
            )
        }
    }
}

fn parse_named_struct_field(
    field: &Field,
    container_attrs: &ContainerAttr,
    field_attrs: &FieldAttr,
    crate_ref: &TokenStream,
) -> TokenStream {
    let upsert = upsert_def(field, crate_ref);

    // TODO: flatten + optional
    if field_attrs.flatten {
        return quote! {{
            let ty = #upsert;

            match ty {
                #crate_ref::DataType::Object(#crate_ref::ObjectType { fields, .. }) => fields,
                _ => panic!("Attempted to flatten non-object field")
            }
        }};
    }

    let name_str = sanitise_raw_ident(field.ident.as_ref().unwrap());

    let name_str = match (field_attrs.rename.clone(), container_attrs.rename_all) {
        (Some(name), _) => name,
        (None, Some(inflection)) => inflection.apply(&name_str),
        (None, None) => name_str,
    };

    let optional = field_attrs.optional;

    let ty_ident = &field.ty;

    let ty = match field_attrs.inline {
        true => quote! {{
            let mut ty = #upsert;

            ty.force_inline();

            ty
        }},
        false => {
            quote! {{
                match <#ty_ident>::name() {
                    Some(name) => {
                        if let None = defs.get(&name) {
                            let def = <#ty_ident as #crate_ref::Type>::base(defs);
                            defs.insert(name.to_string(), def);
                        }

                        <#ty_ident>::refr()
                    },
                    None => {
                        <#ty_ident as #crate_ref::Type>::def(defs)
                    },
                }
            }}
        }
    };

    quote! {{
        let ty = #ty;

        vec![#crate_ref::ObjectField {
            name: #name_str.to_string(),
            ty,
            optional: #optional,
        }]
    }}
}

fn parse_tuple_struct_field(
    field: &Field,
    _container_attrs: &ContainerAttr,
    field_attrs: &FieldAttr,
    crate_ref: &TokenStream,
) -> TokenStream {
    let upsert = upsert_def(field, crate_ref);

    // TODO: flatten + optional
    if field_attrs.flatten {
        return quote! {{
            match ty {
                #crate_ref::DataType::Object(ObjectType { fields, .. }) => fields
                    .into_iter()
                    .map(|of| of.ty)
                    .collect::<Vec<#crate_ref::DataType>>(),
                _ => panic!("Attempted to flatten non-object field"),
            }
        }};
    }

    let optional = field_attrs.optional.then(|| {
        quote! {
            ty = #crate_ref::DataType::Nullable(Box::new(def.clone()));
        }
    });

    let upsert = match field_attrs.inline {
        true => upsert,
        false => {
            let ty_ident = &field.ty;
            quote!(
                match <#ty_ident>::name() {
                    Some(_) => <#ty_ident>::refr(),
                    None => #upsert
                }
            )
        }
    };

    quote! {{
        let mut ty = #upsert;

        #optional

        vec![ty]
    }}
}

fn parse_enum(
    enum_name_str: &str,
    enum_ident: &Ident,
    enum_attrs: &EnumAttr,
    _container_attrs: &ContainerAttr,
    generics: &Generics,
    crate_ref: &TokenStream,
    data: &DataEnum,
) -> TokenStream {
    if data.variants.len() == 0 {
        return quote!(#crate_ref::DataType::Primitive(#crate_ref::PrimitiveType::Never));
    }

    let enum_ident = type_ident_with_generics(&enum_ident, &generics);
    let variants = data
        .variants
        .iter()
        .map(|v| {
            let attrs = VariantAttr::from_attrs(&v.attrs).expect("Failed to parse enum attributes");

            (v, attrs)
        })
        .filter(|(_, attrs)| !attrs.skip)
        .map(|(variant, attrs)| {
            let variant_name_str = variant.ident.to_string();

            let variant_name_str = match (attrs.rename.clone(), _container_attrs.rename_all) {
                (Some(name), _) => name,
                (None, Some(inflection)) => inflection.apply(&variant_name_str),
                (None, None) => variant_name_str,
            };

            match &variant.fields {
                Fields::Unit => {
                    quote!(#crate_ref::EnumVariant::Unit(#variant_name_str.to_string()))
                }
                Fields::Unnamed(fields) => {
                    let fields = fields
                        .unnamed
                        .iter()
                        .map(|field| {
                            let upsert = upsert_def(field, crate_ref);
                            let ty_ident = &field.ty;

                            quote!(
                                match <#ty_ident>::name() {
                                    Some(name) => #crate_ref::DataType::Reference(
                                        name.to_string()
                                    ),
                                    None => #upsert
                                }
                            )
                        })
                        .collect::<Vec<_>>();

                    quote!(#crate_ref::EnumVariant::Unnamed(#crate_ref::TupleType {
                        name: #variant_name_str.to_string(),
                        id: std::any::TypeId::of::<#enum_ident>(),
                        fields: vec![#(#fields),*],
                    }))
                }
                Fields::Named(fields) => {
                    let fields = fields
                        .named
                        .iter()
                        .map(|f| {
                            let ident = f.ident.as_ref().expect("Named field has no ident");
                            let upsert = upsert_def(f, crate_ref);

                            let name = sanitise_raw_ident(ident);

                            let ty = &f.ty;
                            let ty = quote!(
                                match <#ty as #crate_ref::Type>::name() {
                                    Some(name) => #crate_ref::DataType::Reference(
                                        name.to_string()
                                    ),
                                    None => #upsert
                                }
                            );

                            quote!(#crate_ref::ObjectField {
                                name: #name.into(),
                                ty: #ty,
                                optional: false,
                            })
                        })
                        .collect::<Vec<_>>();

                    quote!(#crate_ref::EnumVariant::Named(#crate_ref::ObjectType {
                        name: #variant_name_str.to_string(),
                        inline: true,
                        id: std::any::TypeId::of::<#enum_ident>(),
                        fields: vec![#(#fields),*],
                        tag: None
                    }))
                }
            }
        })
        .collect::<Vec<_>>();

    let repr = match enum_attrs
        .tagged()
        .expect("Invalid tag/content combination")
    {
        Tagged::Externally => quote!(External),
        Tagged::Untagged => quote!(Untagged),
        Tagged::Adjacently { tag, content } => {
            quote!(Adjacent { tag: #tag.to_string(), content: #content.to_string() })
        }
        Tagged::Internally { tag } => {
            quote!(Internal { tag: #tag.to_string() })
        }
    };

    quote!(#crate_ref::DataType::Enum(#crate_ref::EnumType {
        name: #enum_name_str.to_string(),
        id: std::any::TypeId::of::<#enum_ident>(),
        variants: vec![#(#variants),*],
        repr: #crate_ref::EnumRepr::#repr,
    }))
}

fn upsert_def(f: &Field, crate_ref: &TokenStream) -> TokenStream {
    let ty = &f.ty;

    quote! {
        match <#ty as #crate_ref::Type>::name() {
            Some(name) => {
                if let Some(def) = defs.get(&name) {
                    def.clone()
                } else {
                    let def = <#ty as #crate_ref::Type>::base(defs);
                    defs.insert(name.to_string(), def.clone());
                    def
                }
            },
            None => <#ty as #crate_ref::Type>::def(defs),
        }
    }
}

fn sanitise_raw_ident(ident: &Ident) -> String {
    let ident = ident.to_string();
    if ident.starts_with("r#") {
        ident.trim_start_matches("r#").to_owned()
    } else {
        ident
    }
}
