use proc_macro2::{Ident, TokenStream};
use quote::ToTokens;
use syn::{Abi, Attribute, FnArg, ImplItemMethod, Item, ItemImpl, ItemMod, ItemStruct, LitStr, parse_quote, Pat, PatIdent, PatType, Signature, Type, TypeReference};
use syn::fold::Fold;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Token;
use syn::token::Extern;

use crate::validation::JNIBridgeModule;
use crate::utils::unique_ident;

pub(crate) struct ModTransformer {
    module: JNIBridgeModule
}

impl ModTransformer {
    pub(crate) fn new(module: JNIBridgeModule) -> Self {
        ModTransformer {
            module
        }
    }

    pub(crate) fn transform_module(&mut self) -> TokenStream {
        let module_decl = self.module.module_decl.clone();
        self.fold_item_mod(module_decl).into_token_stream()
    }
}

impl ModTransformer {
    fn transform_item_impl(&mut self, node: ItemImpl) -> TokenStream {
        let transformed_item_impl = if let Type::Path(p) = &*node.self_ty {
            let struct_name = p.path.segments.last().unwrap().ident.to_string();
            let struct_package = self.module.package_map[&struct_name].clone();
            let mut impl_transformer = ImplFnTransformer { struct_name, package: struct_package };

            impl_transformer.fold_item_impl(node)
        } else {
            ItemImpl {
                attrs: node.attrs.into_iter().map(|a| self.fold_attribute(a)).collect(),
                defaultness: node.defaultness,
                unsafety: node.unsafety,
                impl_token: node.impl_token,
                generics: self.fold_generics(node.generics),
                trait_: node.trait_,
                self_ty: Box::new(self.fold_type(*node.self_ty)),
                brace_token: node.brace_token,
                items: node.items.into_iter().map(|i| self.fold_impl_item(i)).collect(),
            }
        };

        transformed_item_impl.items.iter()
            .map(|i| i.to_token_stream())
            .fold(TokenStream::new(), |item, mut stream| {
                item.to_tokens(&mut stream);
                stream
            })
    }
}

impl Fold for ModTransformer {
    fn fold_item(&mut self, node: Item) -> Item {
        match node {
            Item::Const(c) => Item::Const(self.fold_item_const(c)),
            Item::Enum(e) => Item::Enum(self.fold_item_enum(e)),
            Item::ExternCrate(c) => Item::ExternCrate(self.fold_item_extern_crate(c)),
            Item::Fn(f) => Item::Fn(self.fold_item_fn(f)),
            Item::ForeignMod(m) => Item::ForeignMod(self.fold_item_foreign_mod(m)),
            Item::Impl(i) => {
                Item::Verbatim(self.transform_item_impl(i))
            }
            Item::Macro(m) => Item::Macro(self.fold_item_macro(m)),
            Item::Macro2(m) => Item::Macro2(self.fold_item_macro2(m)),
            Item::Mod(m) => Item::Mod(self.fold_item_mod(m)),
            Item::Static(s) => Item::Static(self.fold_item_static(s)),
            Item::Struct(s) => Item::Struct(self.fold_item_struct(s)),
            Item::Trait(t) => Item::Trait(self.fold_item_trait(t)),
            Item::TraitAlias(t) => Item::TraitAlias(self.fold_item_trait_alias(t)),
            Item::Type(t) => Item::Type(self.fold_item_type(t)),
            Item::Union(u) => Item::Union(self.fold_item_union(u)),
            Item::Use(u) => Item::Use(self.fold_item_use(u)),
            Item::Verbatim(_) => node,
            _ => node,
        }
    }

    fn fold_item_mod(&mut self, node: ItemMod) -> ItemMod {
        let allow_non_snake_case: Attribute = parse_quote! { #![allow(non_snake_case)] };
        let allow_unused: Attribute = parse_quote! { #![allow(unused)] };

        ItemMod {
            attrs: vec![allow_non_snake_case, allow_unused],
            vis: self.fold_visibility(node.vis),
            mod_token: node.mod_token,
            ident: self.fold_ident(node.ident),
            content: node.content.map(|(brace, items)| (brace, items.into_iter().map(|i| self.fold_item(i)).collect())),
            semi: node.semi,
        }
    }

    fn fold_item_struct(&mut self, node: ItemStruct) -> ItemStruct {
        ItemStruct {
            attrs: vec![],
            vis: node.vis,
            struct_token: node.struct_token,
            ident: node.ident,
            generics: self.fold_generics(node.generics),
            fields: self.fold_fields(node.fields),
            semi_token: node.semi_token,
        }
    }
}

struct ImplFnTransformer {
    pub(crate) struct_name: String,
    pub(crate) package: String,
}

impl Fold for ImplFnTransformer {
    fn fold_impl_item_method(&mut self, node: ImplItemMethod) -> ImplItemMethod {
        let no_mangle = parse_quote! { #[no_mangle] };
        ImplItemMethod {
            attrs: vec![no_mangle],
            vis: node.vis,
            defaultness: node.defaultness,
            sig: self.fold_signature(node.sig),
            block: self.fold_block(node.block),
        }
    }

    fn fold_signature(&mut self, node: Signature) -> Signature {
        let jni_method_name = {
            let snake_case_package = self.package.clone().replace('.', "_");
            format!("{}_{}_{}", snake_case_package, self.struct_name, node.ident.to_string())
        };

        let new_inputs: Punctuated<_, _> = node.inputs.iter()
            .map(|arg| {
                match arg {
                    FnArg::Receiver(r) => {
                        let receiver_span = r.span();
                        let struct_type_ident = Type::Verbatim(Ident::new(&self.struct_name, receiver_span).to_token_stream());

                        let self_type = match r.reference.clone() {
                            Some((and_token, lifetime)) => {
                                Type::Reference(TypeReference {
                                    and_token,
                                    lifetime,
                                    mutability: r.mutability,
                                    elem: Box::new(struct_type_ident),
                                })
                            }

                            None => Type::Verbatim(struct_type_ident.to_token_stream())
                        };

                        FnArg::Typed(PatType {
                            attrs: r.attrs.clone(),
                            pat: Box::new(Pat::Ident(PatIdent {
                                attrs: vec![],
                                by_ref: None,
                                mutability: None,
                                ident: unique_ident(&format!("receiver_{}", self.struct_name), receiver_span),
                                subpat: None,
                            })),
                            colon_token: Token![:](receiver_span),
                            ty: Box::new(self_type),
                        })
                    }

                    FnArg::Typed(t) => {
                        if let Pat::Ident(ident) = &*t.pat {
                            if ident.ident == "self" {
                                FnArg::Typed(PatType {
                                    attrs: vec![],
                                    pat: Box::new(Pat::Ident(PatIdent {
                                        attrs: ident.attrs.clone(),
                                        by_ref: ident.by_ref,
                                        mutability: ident.mutability,
                                        ident: unique_ident(&format!("receiver_{}", self.struct_name), t.span()),
                                        subpat: ident.subpat.clone()
                                    })),
                                    colon_token: t.colon_token,
                                    ty: t.ty.clone()
                                })
                            } else {
                                arg.clone()
                            }
                        } else {
                            arg.clone()
                        }
                    }
                }
            })
            .collect();

        Signature {
            constness: node.constness,
            asyncness: node.asyncness,
            unsafety: node.unsafety,
            abi: Some(Abi {
                extern_token: Extern { span: node.span() },
                name: Some(LitStr::new("system", node.span())),
            }),
            fn_token: node.fn_token,
            ident: Ident::new(&jni_method_name, node.ident.span()),
            generics: node.generics,
            paren_token: node.paren_token,
            inputs: new_inputs,
            variadic: node.variadic,
            output: node.output,
        }
    }
}