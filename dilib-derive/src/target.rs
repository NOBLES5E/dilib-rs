use crate::dependency::{Dependency, Scope, TargetField};
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{
    Data, DataStruct, DeriveInput, Field, Fields, GenericArgument, Ident, PathArguments, Type,
};

pub struct InjectableTarget {
    target_type: Ident,
    container: Ident,
    constructor: Option<TargetConstructor>,
    deps: Vec<Dependency>,
    is_unit: bool,
}

pub struct TargetConstructor {
    name: String,
    params: Vec<String>,
}

impl InjectableTarget {
    pub fn new(
        target_type: Ident,
        container: Ident,
        constructor: Option<TargetConstructor>,
        deps: Vec<Dependency>,
        is_unit: bool,
    ) -> Self {
        InjectableTarget {
            target_type,
            container,
            constructor,
            deps,
            is_unit,
        }
    }

    /// Returns `true` if the target type is using the unit syntax `Struct` and not `Struct{}`.
    fn is_unit(&self) -> bool {
        self.is_unit
    }

    pub fn emit(&self) -> proc_macro2::TokenStream {
        let target_type = &self.target_type;

        if self.is_unit {
            return quote! {
                impl dilib::Injectable for #target_type {
                    fn resolve(_: &dilib::Container) -> Self {
                        #target_type
                    }
                }
            };
        }

        let container = &self.container;
        let deps = self.deps.as_slice();

        let create_instance = if let Some(constructor) = &self.constructor {
            let params = constructor
                .params
                .iter()
                .map(|s| Ident::new(s, Span::call_site()));

            // Type :: constructor ( params )
            let constructor_name = Ident::new(&constructor.name, Span::call_site());
            quote! { #target_type :: #constructor_name ( #(#params)* )}
        } else {
            let params = deps.iter().map(|s| s.var_name());

            // Type { params }
            quote! { #target_type { #(#params),* } }
        };

        quote! {
            impl dilib::Injectable for #target_type {
                fn resolve(#container : &dilib::Container) -> Self {
                    #(#deps)*
                    #create_instance
                }
            }
        }
    }
}

pub fn parse_derive_injectable(input: DeriveInput) -> InjectableTarget {
    match &input.data {
        Data::Enum(_) => panic!("Enum types cannot implement `Injectable` with #[derive]"),
        Data::Union(_) => panic!("Union types cannot implement `Injectable` with #[derive]"),
        Data::Struct(data_struct) => {
            let target_type = input.ident.clone();
            let constructor = get_target_constructor(&input);
            let container = get_container_identifier(data_struct);
            let deps = get_deps(&data_struct.fields);
            let is_unit = data_struct.fields == Fields::Unit;

            InjectableTarget::new(target_type, container, constructor, deps, is_unit)
        }
    }
}

fn get_target_constructor(_input: &DeriveInput) -> Option<TargetConstructor> {
    // todo
    None
}

fn get_container_identifier(struct_data: &DataStruct) -> Ident {
    const CONTAINER_IDENT: &str = "container";

    match &struct_data.fields {
        Fields::Named(fields) => {
            let mut container_name = CONTAINER_IDENT.to_owned();
            let mut matches = 1_usize;

            for f in &fields.named {
                let field_name = f.ident.as_ref().unwrap().to_string();

                // We prevent name collisions
                // If there is a field named `container` we try other name
                // following the pattern: `container(N)` where N is the number of collisions
                if field_name == container_name {
                    container_name = format!("{}{}", container_name, matches);
                    matches += 1;
                }
            }

            Ident::new(container_name.as_str(), Span::call_site())
        }
        Fields::Unnamed(_) => Ident::new(CONTAINER_IDENT, Span::call_site()),
        Fields::Unit => Ident::new("_", Span::call_site()),
    }
}

fn get_deps(fields: &Fields) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let container = Ident::new("container", Span::call_site());

    // todo: check for attributes for additional config

    match fields {
        Fields::Unit => deps,
        Fields::Named(fields_named) => {
            for f in &fields_named.named {
                let field = TargetField::Named(f.ident.clone().unwrap());
                let (field_type, scope) = get_type_and_scope(&f.ty);
                let dependency = Dependency::new(
                    field,
                    field_type,
                    scope,
                    container.clone()
                );

                deps.push(dependency);
            }

            deps
        }
        Fields::Unnamed(fields_unnamed) => {
            for (index, f) in fields_unnamed.unnamed.iter().enumerate() {
                let field = TargetField::Unnamed(index);
                let (field_type, scope) = get_type_and_scope(&f.ty);
                let dependency = Dependency::new(
                    field,
                    field_type,
                    scope,
                    container.clone()
                );

                deps.push(dependency);
            }

            deps
        }
    }
}

fn get_type_and_scope(ty: &Type) -> (Type, Scope) {
    if let Some(generic) = get_singleton_type(ty) {
        (generic, Scope::Singleton)
    } else {
        (ty.clone(), Scope::Scoped)
    }
}

fn get_singleton_type(ty: &Type) -> Option<Type> {
    match ty {
        Type::Path(type_path) => {
            // Is declared as <T as Trait>::Inner
            if type_path.qself.is_some() {
                return None;
            }

            // todo: We are not checking full paths like: dilib::Singleton<T>

            let raw = type_path.path.to_token_stream().to_string();
            let s = raw.split_ascii_whitespace().collect::<String>();

            // SAFETY: A type path should have at least 1 element
            let segment = type_path.path.segments.last().unwrap();
            let ident = segment.ident.to_string();

            // Is `Singleton<T>`
            if ident == "Singleton" && !segment.arguments.is_empty() {
                match &segment.arguments {
                    PathArguments::AngleBracketed(bracketed) => {
                        let generic_arg = bracketed.args.first().unwrap();
                        if let GenericArgument::Type(Type::Path(generic_type)) = generic_arg {
                            return Some(Type::Path(generic_type.clone()));
                        }
                    }
                    _ => {}
                }
            }

            // Is `Arc<Mutex<T>>`
            if ident == "Arc" {
                match &segment.arguments {
                    PathArguments::AngleBracketed(bracket) => {
                        let generic_arg = bracket.args.first().unwrap();
                        if let GenericArgument::Type(Type::Path(generic)) = generic_arg {
                            let inner = generic.path.segments.last().unwrap();
                            if inner.ident.to_string() == "Mutex" {
                                return Some(Type::Path(generic.clone()));
                            }
                        }
                    }
                    _ => {}
                }
            }

            None
        }
        _ => None,
    }
}

