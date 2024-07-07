use proc_macro::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;

macro_rules! bail {
    (span=$span:expr, $($tt:tt)*) => {
        return syn::Error::new_spanned($span, format!($($tt)*))
            .into_compile_error()
            .into()
    };

    ($($tt:tt)*) => {
        return syn::Error::new(format!($($tt)*))
    };
}

#[proc_macro_derive(Trace, attributes(trace))]
pub fn derive_trace(tokens: TokenStream) -> TokenStream {
    let syn::DeriveInput {
        ident,
        generics,
        data,
        ..
    } = syn::parse_macro_input!(tokens as syn::DeriveInput);

    let body = match data {
        syn::Data::Struct(syn::DataStruct { fields, .. }) => {
            let fields = fields
                .into_iter()
                .map(|syn::Field { ident, .. }| quote!(self.#ident.trace()));
            quote! {
                #(#fields;)*
            }
        }
        syn::Data::Enum(syn::DataEnum { variants, .. }) => {
            let variants = variants
                .into_iter()
                .map(|syn::Variant { ident, fields, .. }| match fields {
                    syn::Fields::Named(syn::FieldsNamed { named, .. }) => {
                        let traced = named
                            .iter()
                            .map(|syn::Field { ident, .. }| quote!(#ident.trace()));
                        quote! {
                            Self::#ident { #named } => {
                                #(#traced;),*
                            }
                        }
                    }
                    syn::Fields::Unnamed(_) => todo!(),
                    syn::Fields::Unit => todo!(),
                });

            quote! {
                match self {
                    #(#variants)*
                }
            }
        }
        syn::Data::Union(v) => {
            bail!(span = v.union_token, "cannot derive `Trace` on a union")
        }
    };

    let mut bounds = generics.clone();
    let params = generics;

    for type_param in bounds.type_params_mut() {
        type_param
            .bounds
            .push(syn::TypeParamBound::Trait(syn::TraitBound {
                paren_token: None,
                modifier: syn::TraitBoundModifier::None,
                lifetimes: None,
                path: syn::Path {
                    leading_colon: Some(syn::token::PathSep::default()),
                    segments: Punctuated::from_iter([
                        syn::PathSegment {
                            ident: quote::format_ident!("gc"),
                            arguments: syn::PathArguments::None,
                        },
                        syn::PathSegment {
                            ident: quote::format_ident!("Trace"),
                            arguments: syn::PathArguments::None,
                        },
                    ]),
                },
            }));
    }

    quote! {
        unsafe impl #bounds ::gc::Trace for #ident #params {
            unsafe fn trace(&self) {
                #body
            }
        }
    }
    .into()
}
