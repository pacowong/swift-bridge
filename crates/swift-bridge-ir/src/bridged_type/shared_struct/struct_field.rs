use std::fmt::{Debug, Formatter};

use proc_macro2::{Ident, TokenStream};
use quote::{quote, ToTokens};
use syn::Type;

pub(crate) use self::normalized_field::*;

mod normalized_field;

#[derive(Debug, PartialEq, Clone)]
pub(crate) enum StructFields {
    Named(Vec<NamedStructField>),
    Unnamed(Vec<UnnamedStructField>),
    Unit,
}

impl StructFields {
    /// Returns true if the struct does not have any named or unnamed fields.
    pub fn is_empty(&self) -> bool {
        match self {
            StructFields::Named(named) => named.is_empty(),
            StructFields::Unnamed(unnamed) => unnamed.is_empty(),
            StructFields::Unit => true,
        }
    }

    pub fn normalized_fields(&self) -> Vec<NormalizedStructField> {
        match self {
            StructFields::Named(named) => named
                .iter()
                .map(|n| NormalizedStructField {
                    accessor: NormalizedStructFieldAccessor::Named(n.name.clone()),
                    ty: n.ty.clone(),
                })
                .collect(),
            StructFields::Unnamed(unnamed) => unnamed
                .iter()
                .map(|u| NormalizedStructField {
                    accessor: NormalizedStructFieldAccessor::Unnamed(u.idx),
                    ty: u.ty.clone(),
                })
                .collect(),
            StructFields::Unit => Vec::new(),
        }
    }

    pub fn wrap_declaration_fields(&self, struct_fields: &[TokenStream]) -> TokenStream {
        match &self {
            StructFields::Named(_) => {
                quote! {
                    { #(#struct_fields),* }
                }
            }
            StructFields::Unnamed(_) => {
                quote! {
                    ( #(#struct_fields),* );
                }
            }
            StructFields::Unit => {
                quote! { ; }
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct NamedStructField {
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone)]
pub(crate) struct UnnamedStructField {
    pub ty: Type,
    pub idx: usize,
}

impl NamedStructField {
    pub fn swift_name_string(&self) -> String {
        self.name.to_string()
    }
}

impl UnnamedStructField {
    pub fn swift_name_string(&self) -> String {
        format!("_{}", self.idx)
    }
}

impl PartialEq for NamedStructField {
    fn eq(&self, other: &Self) -> bool {
        self.name.to_string() == other.name.to_string()
            && self.ty.to_token_stream().to_string() == other.ty.to_token_stream().to_string()
    }
}

impl Debug for NamedStructField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedStructField")
            .field("name", &self.name.to_string())
            .field("ty", &self.ty.to_token_stream())
            .finish()
    }
}

impl PartialEq for UnnamedStructField {
    fn eq(&self, other: &Self) -> bool {
        self.ty.to_token_stream().to_string() == other.ty.to_token_stream().to_string()
            && self.idx == other.idx
    }
}

impl Debug for UnnamedStructField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnnamedStructField")
            .field("ty", &self.ty.to_token_stream())
            .field("idx", &self.idx)
            .finish()
    }
}