// Copyright 2018-2020 Parity Technologies
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Various internal utils.
//!
//! NOTE: attributes finder must be checked using check_attribute first,
//! otherwise the macro can panic.

use std::str::FromStr;

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{
	parse::Parse, punctuated::Punctuated, spanned::Spanned, token, Attribute, Data, DeriveInput,
	Field, Fields, FieldsNamed, FieldsUnnamed, Lit, Meta, MetaNameValue, NestedMeta, Path, Variant, Expr,
};

fn find_meta_item<'a, F, R, I, M>(mut itr: I, mut pred: F) -> Option<R>
where
	F: FnMut(M) -> Option<R> + Clone,
	I: Iterator<Item = &'a Attribute>,
	M: Parse,
{
	itr.find_map(|attr| {
		attr.path.is_ident("codec").then(|| pred(attr.parse_args().ok()?)).flatten()
	})
}

/// Look for a `#[codec(index = $int)]` attribute on a variant. If no attribute
/// is found, fall back to the discriminant or just the variant index.
pub fn variant_index(
    variant: &Variant,
    i: usize,
    collected_const_indices: &mut Vec<TokenStream>,
) -> TokenStream {
    let mut index_option: Option<TokenStream> = None;

    for attr in variant.attrs.iter().filter(|attr| attr.path.is_ident("codec")) {
        if let Ok(codec_variants) = attr.parse_args::<CodecVariants>() {
            if let Some(codec_index) = codec_variants.index {
                match codec_index {
                    CodecIndex::U8(value) => {
                        index_option = Some(quote! { #value });
                        break;
                    },
                    CodecIndex::ExprConst(expr) => {
                        collected_const_indices.push(quote! { #expr });
                        index_option = Some(quote! { #expr });
                        break;
                    }
                }
            }
        }
    }

    // Fallback to discriminant or index
    index_option.unwrap_or_else(|| {
        variant
            .discriminant
            .as_ref()
            .map(|(_, expr)| quote! { #expr })
            .unwrap_or_else(|| quote! { #i })
    })
}

/// Look for a `#[codec(encoded_as = "SomeType")]` outer attribute on the given
/// `Field`.
pub fn get_encoded_as_type(field: &Field) -> Option<TokenStream> {
	find_meta_item(field.attrs.iter(), |meta| {
		if let NestedMeta::Meta(Meta::NameValue(ref nv)) = meta {
			if nv.path.is_ident("encoded_as") {
				if let Lit::Str(ref s) = nv.lit {
					return Some(
						TokenStream::from_str(&s.value())
							.expect("Internal error, encoded_as attribute must have been checked"),
					)
				}
			}
		}

		None
	})
}

/// Look for a `#[codec(compact)]` outer attribute on the given `Field`.
pub fn is_compact(field: &Field) -> bool {
	find_meta_item(field.attrs.iter(), |meta| {
		if let NestedMeta::Meta(Meta::Path(ref path)) = meta {
			if path.is_ident("compact") {
				return Some(())
			}
		}

		None
	})
	.is_some()
}

/// Look for a `#[codec(skip)]` in the given attributes.
pub fn should_skip(attrs: &[Attribute]) -> bool {
	find_meta_item(attrs.iter(), |meta| {
		if let NestedMeta::Meta(Meta::Path(ref path)) = meta {
			if path.is_ident("skip") {
				return Some(path.span())
			}
		}

		None
	})
	.is_some()
}

/// Look for a `#[codec(dumb_trait_bound)]`in the given attributes.
pub fn has_dumb_trait_bound(attrs: &[Attribute]) -> bool {
	find_meta_item(attrs.iter(), |meta| {
		if let NestedMeta::Meta(Meta::Path(ref path)) = meta {
			if path.is_ident("dumb_trait_bound") {
				return Some(())
			}
		}

		None
	})
	.is_some()
}

/// Generate the crate access for the crate using 2018 syntax.
fn crate_access() -> syn::Result<proc_macro2::Ident> {
	use proc_macro2::{Ident, Span};
	use proc_macro_crate::{crate_name, FoundCrate};
	const DEF_CRATE: &str = "parity-scale-codec";
	match crate_name(DEF_CRATE) {
		Ok(FoundCrate::Itself) => {
			let name = DEF_CRATE.to_string().replace('-', "_");
			Ok(syn::Ident::new(&name, Span::call_site()))
		},
		Ok(FoundCrate::Name(name)) => Ok(Ident::new(&name, Span::call_site())),
		Err(e) => Err(syn::Error::new(Span::call_site(), e)),
	}
}

/// This struct matches `crate = ...` where the ellipsis is a `Path`.
struct CratePath {
	_crate_token: Token![crate],
	_eq_token: Token![=],
	path: Path,
}

impl Parse for CratePath {
	fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
		Ok(CratePath {
			_crate_token: input.parse()?,
			_eq_token: input.parse()?,
			path: input.parse()?,
		})
	}
}

impl From<CratePath> for Path {
	fn from(CratePath { path, .. }: CratePath) -> Self {
		path
	}
}

/// Match `#[codec(crate = ...)]` and return the `...` if it is a `Path`.
fn codec_crate_path_inner(attr: &Attribute) -> Option<Path> {
	// match `#[codec ...]`
	attr.path
		.is_ident("codec")
		.then(|| {
			// match `#[codec(crate = ...)]` and return the `...`
			attr.parse_args::<CratePath>().map(Into::into).ok()
		})
		.flatten()
}

/// Match `#[codec(crate = ...)]` and return the ellipsis as a `Path`.
///
/// If not found, returns the default crate access pattern.
///
/// If multiple items match the pattern, all but the first are ignored.
pub fn codec_crate_path(attrs: &[Attribute]) -> syn::Result<Path> {
	match attrs.iter().find_map(codec_crate_path_inner) {
		Some(path) => Ok(path),
		None => crate_access().map(|ident| parse_quote!(::#ident)),
	}
}

/// Parse `name(T: Bound, N: Bound)` or `name(skip_type_params(T, N))` as a custom trait bound.
pub enum CustomTraitBound<N> {
	SpecifiedBounds {
		_name: N,
		_paren_token: token::Paren,
		bounds: Punctuated<syn::WherePredicate, Token![,]>,
	},
	SkipTypeParams {
		_name: N,
		_paren_token_1: token::Paren,
		_skip_type_params: skip_type_params,
		_paren_token_2: token::Paren,
		type_names: Punctuated<syn::Ident, Token![,]>,
	},
}

impl<N: Parse> Parse for CustomTraitBound<N> {
	fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
		let mut content;
		let _name: N = input.parse()?;
		let _paren_token = syn::parenthesized!(content in input);
		if content.peek(skip_type_params) {
			Ok(Self::SkipTypeParams {
				_name,
				_paren_token_1: _paren_token,
				_skip_type_params: content.parse::<skip_type_params>()?,
				_paren_token_2: syn::parenthesized!(content in content),
				type_names: content.parse_terminated(syn::Ident::parse)?,
			})
		} else {
			Ok(Self::SpecifiedBounds {
				_name,
				_paren_token,
				bounds: content.parse_terminated(syn::WherePredicate::parse)?,
			})
		}
	}
}

syn::custom_keyword!(encode_bound);
syn::custom_keyword!(decode_bound);
syn::custom_keyword!(mel_bound);
syn::custom_keyword!(skip_type_params);

/// Look for a `#[codec(decode_bound(T: Decode))]` in the given attributes.
///
/// If found, it should be used as trait bounds when deriving the `Decode` trait.
pub fn custom_decode_trait_bound(attrs: &[Attribute]) -> Option<CustomTraitBound<decode_bound>> {
	find_meta_item(attrs.iter(), Some)
}

/// Look for a `#[codec(encode_bound(T: Encode))]` in the given attributes.
///
/// If found, it should be used as trait bounds when deriving the `Encode` trait.
pub fn custom_encode_trait_bound(attrs: &[Attribute]) -> Option<CustomTraitBound<encode_bound>> {
	find_meta_item(attrs.iter(), Some)
}

/// Look for a `#[codec(mel_bound(T: MaxEncodedLen))]` in the given attributes.
///
/// If found, it should be used as the trait bounds when deriving the `MaxEncodedLen` trait.
#[cfg(feature = "max-encoded-len")]
pub fn custom_mel_trait_bound(attrs: &[Attribute]) -> Option<CustomTraitBound<mel_bound>> {
	find_meta_item(attrs.iter(), Some)
}

/// Given a set of named fields, return an iterator of `Field` where all fields
/// marked `#[codec(skip)]` are filtered out.
pub fn filter_skip_named(fields: &syn::FieldsNamed) -> impl Iterator<Item = &Field> {
	fields.named.iter().filter(|f| !should_skip(&f.attrs))
}

/// Given a set of unnamed fields, return an iterator of `(index, Field)` where all fields
/// marked `#[codec(skip)]` are filtered out.
pub fn filter_skip_unnamed(
	fields: &syn::FieldsUnnamed,
) -> impl Iterator<Item = (usize, &Field)> {
	fields.unnamed.iter().enumerate().filter(|(_, f)| !should_skip(&f.attrs))
}

/// Ensure attributes are correctly applied. This *must* be called before using
/// any of the attribute finder methods or the macro may panic if it encounters
/// misapplied attributes.
///
/// The top level can have the following attributes:
///
/// * `#[codec(dumb_trait_bound)]`
/// * `#[codec(encode_bound(T: Encode))]`
/// * `#[codec(decode_bound(T: Decode))]`
/// * `#[codec(mel_bound(T: MaxEncodedLen))]`
/// * `#[codec(crate = path::to::crate)]
///
/// Fields can have the following attributes:
///
/// * `#[codec(skip)]`
/// * `#[codec(compact)]`
/// * `#[codec(encoded_as = "$EncodeAs")]` with $EncodedAs a valid TokenStream
///
/// Variants can have the following attributes:
///
/// * `#[codec(skip)]`
/// * `#[codec(index = $int)]`
pub fn check_attributes(input: &DeriveInput) -> syn::Result<()> {
	for attr in &input.attrs {
		check_top_attribute(attr)?;
	}

	match input.data {
		Data::Struct(ref data) => match &data.fields {
			| Fields::Named(FieldsNamed { named: fields, .. }) |
			Fields::Unnamed(FieldsUnnamed { unnamed: fields, .. }) =>
				for field in fields {
					for attr in &field.attrs {
						check_field_attribute(attr)?;
					}
				},
			Fields::Unit => (),
		},
		Data::Enum(ref data) =>
			for variant in data.variants.iter() {
				for attr in &variant.attrs {
					check_variant_attribute(attr)?;
				}
				for field in &variant.fields {
					for attr in &field.attrs {
						check_field_attribute(attr)?;
					}
				}
			},
		Data::Union(_) => (),
	}
	Ok(())
}

// Check if the attribute is `#[allow(..)]`, `#[deny(..)]`, `#[forbid(..)]` or `#[warn(..)]`.
pub fn is_lint_attribute(attr: &Attribute) -> bool {
	attr.path.is_ident("allow") ||
		attr.path.is_ident("deny") ||
		attr.path.is_ident("forbid") ||
		attr.path.is_ident("warn")
}

// Ensure a field is decorated only with the following attributes:
// * `#[codec(skip)]`
// * `#[codec(compact)]`
// * `#[codec(encoded_as = "$EncodeAs")]` with $EncodedAs a valid TokenStream
fn check_field_attribute(attr: &Attribute) -> syn::Result<()> {
	let field_error = "Invalid attribute on field, only `#[codec(skip)]`, `#[codec(compact)]` and \
		`#[codec(encoded_as = \"$EncodeAs\")]` are accepted.";

	if attr.path.is_ident("codec") {
		match attr.parse_meta()? {
			Meta::List(ref meta_list) if meta_list.nested.len() == 1 => {
				match meta_list.nested.first().expect("Just checked that there is one item; qed") {
					NestedMeta::Meta(Meta::Path(path))
						if path.get_ident().map_or(false, |i| i == "skip") =>
						Ok(()),

					NestedMeta::Meta(Meta::Path(path))
						if path.get_ident().map_or(false, |i| i == "compact") =>
						Ok(()),

					NestedMeta::Meta(Meta::NameValue(MetaNameValue {
						path,
						lit: Lit::Str(lit_str),
						..
					})) if path.get_ident().map_or(false, |i| i == "encoded_as") =>
						TokenStream::from_str(&lit_str.value())
							.map(|_| ())
							.map_err(|_e| syn::Error::new(lit_str.span(), "Invalid token stream")),

					elt => Err(syn::Error::new(elt.span(), field_error)),
				}
			},
			meta => Err(syn::Error::new(meta.span(), field_error)),
		}
	} else {
		Ok(())
	}
}

pub enum CodecIndex {
    U8(u8),
    ExprConst(Expr),
}

struct CodecVariants {
	skip: bool,
    index: Option<CodecIndex>,
}

const INDEX_RANGE_ERROR: &str = "Index attribute variant must be in 0..255";
const INDEX_TYPE_ERROR: &str =
	"Only u8 indices are accepted for attribute variant `#[codec(index = $u8)]`";
const ATTRIBUTE_ERROR: &str =
	"Invalid attribute on variant, only `#[codec(skip)]` and `#[codec(index = $u8)]` are accepted.";

impl Parse for CodecVariants {
	fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
		let mut skip = false;
		let mut index = None;

		while !input.is_empty() {
			let lookahead = input.lookahead1();
			if lookahead.peek(syn::Ident) {
				let ident: syn::Ident = input.parse()?;
				if ident == "skip" {
					skip = true;
				} else if ident == "index" {
					input.parse::<Token![=]>()?;
					if let Ok(lit) = input.parse::<syn::LitInt>() {
						let parsed_index = lit
							.base10_parse::<u8>()
							.map_err(|_| syn::Error::new(lit.span(), INDEX_RANGE_ERROR));
						index = Some(CodecIndex::U8(parsed_index?));
					} else {
						let expr = input
							.parse::<syn::Expr>()
							.map_err(|_| syn::Error::new_spanned(ident, INDEX_TYPE_ERROR))?;
						index = Some(CodecIndex::ExprConst(expr));
					}
				} else {
					return Err(syn::Error::new_spanned(ident, ATTRIBUTE_ERROR));
				}
			} else {
				return Err(lookahead.error());
			}
		}

		Ok(CodecVariants { skip, index })
	}
}

// Ensure a field is decorated only with the following attributes:
// * `#[codec(skip)]`
// * `#[codec(index = $int)]`
fn check_variant_attribute(attr: &Attribute) -> syn::Result<()> {
	if attr.path.is_ident("codec") {
		match attr.parse_args::<CodecVariants>() {
			Ok(codec_variants) => {
				if codec_variants.skip || codec_variants.index.is_some() {
					Ok(())
				} else {
					Err(syn::Error::new_spanned(attr, ATTRIBUTE_ERROR))
				}
			},
			Err(e) => Err(syn::Error::new_spanned(
				attr,
				format!("Error checking variant attribute: {}", e),
			)),
		}
	} else {
		Ok(())
	}
}

// Only `#[codec(dumb_trait_bound)]` is accepted as top attribute
fn check_top_attribute(attr: &Attribute) -> syn::Result<()> {
	let top_error = "Invalid attribute: only `#[codec(dumb_trait_bound)]`, \
		`#[codec(crate = path::to::crate)]`, `#[codec(encode_bound(T: Encode))]`, \
		`#[codec(decode_bound(T: Decode))]`, or `#[codec(mel_bound(T: MaxEncodedLen))]` \
		are accepted as top attribute";
	if attr.path.is_ident("codec") &&
		attr.parse_args::<CustomTraitBound<encode_bound>>().is_err() &&
		attr.parse_args::<CustomTraitBound<decode_bound>>().is_err() &&
		attr.parse_args::<CustomTraitBound<mel_bound>>().is_err() &&
		codec_crate_path_inner(attr).is_none()
	{
		match attr.parse_meta()? {
			Meta::List(ref meta_list) if meta_list.nested.len() == 1 => {
				match meta_list.nested.first().expect("Just checked that there is one item; qed") {
					NestedMeta::Meta(Meta::Path(path))
						if path.get_ident().map_or(false, |i| i == "dumb_trait_bound") =>
						Ok(()),

					elt => Err(syn::Error::new(elt.span(), top_error)),
				}
			},
			_ => Err(syn::Error::new(attr.span(), top_error)),
		}
	} else {
		Ok(())
	}
}

fn check_repr(attrs: &[syn::Attribute], value: &str) -> bool {
	let mut result = false;
	for raw_attr in attrs {
		let path = raw_attr.path.clone().into_token_stream().to_string();
		if path != "repr" {
			continue;
		}

		result = raw_attr.tokens.clone().into_token_stream().to_string() == value;
	}

	result
}

/// Checks whether the given attributes contain a `#[repr(transparent)]`.
pub fn is_transparent(attrs: &[syn::Attribute]) -> bool {
	// TODO: When migrating to syn 2 the `"(transparent)"` needs to be changed into `"transparent"`.
	check_repr(attrs, "(transparent)")
}

/// Find a duplicate `TokenStream` in a list of indices.
/// Each `TokenStream` is a constant expression expected to represent an u8 index for an enum variant.
pub fn find_const_duplicate(indices: &[TokenStream]) -> Option<(TokenStream, String)> {
    let mut seen = std::collections::HashSet::new();
    for index in indices {
        let token_str = index.to_token_stream().to_string();
        if !seen.insert(token_str.clone()) {
            return Some((index.clone(), token_str));
        }
    }
    None
}
