use crate::bridged_type::{fn_arg_name, BridgedType, StdLibType, StructSwiftRepr, TypePosition};
use crate::codegen::generate_swift::vec::generate_vectorizable_extension;
use crate::parse::{
    HostLang, OpaqueForeignTypeDeclaration, SharedTypeDeclaration, TypeDeclaration,
    TypeDeclarations,
};
use crate::parsed_extern_fn::ParsedExternFn;
use crate::{SwiftBridgeModule, SWIFT_BRIDGE_PREFIX};
use quote::ToTokens;
use std::collections::HashMap;
use std::ops::Deref;
use syn::{Path, ReturnType, Type};

mod option;
mod vec;

impl SwiftBridgeModule {
    /// Generate the corresponding Swift code for the bridging module.
    pub fn generate_swift(&self) -> String {
        let mut swift = "".to_string();

        let mut associated_funcs_and_methods: HashMap<String, Vec<&ParsedExternFn>> =
            HashMap::new();

        for function in &self.functions {
            if function.host_lang.is_rust() {
                if let Some(ty) = function.associated_type.as_ref() {
                    match ty {
                        TypeDeclaration::Shared(_) => {
                            //
                            todo!("Think about what to do here..")
                        }
                        TypeDeclaration::Opaque(ty) => {
                            associated_funcs_and_methods
                                .entry(ty.ident.to_string())
                                .or_default()
                                .push(function);
                        }
                    };
                    continue;
                }
            }

            let func_definition = match function.host_lang {
                HostLang::Rust => {
                    gen_func_swift_calls_rust(function, &self.types, &self.swift_bridge_path)
                }
                HostLang::Swift => gen_function_exposes_swift_to_rust(
                    function,
                    &self.types,
                    &self.swift_bridge_path,
                ),
            };

            swift += &func_definition;
            swift += "\n";
        }

        for ty in self.types.types() {
            match ty {
                TypeDeclaration::Shared(SharedTypeDeclaration::Struct(shared_struct)) => {
                    match shared_struct.swift_repr {
                        StructSwiftRepr::Class => {
                            todo!()
                        }
                        StructSwiftRepr::Structure => {
                            // No need to generate any code. Swift will automatically generate a
                            //  struct from our C header typedef that we generate for this struct.

                            continue;
                        }
                    }
                }
                TypeDeclaration::Opaque(ty) => match ty.host_lang {
                    HostLang::Rust => {
                        swift += &generate_swift_class(
                            ty,
                            &associated_funcs_and_methods,
                            &self.types,
                            &self.swift_bridge_path,
                        );
                        swift += "\n";

                        swift += &generate_vectorizable_extension(&ty.ident);
                        swift += "\n";
                    }
                    HostLang::Swift => {
                        swift += &generate_drop_swift_instance_reference_count(ty);
                        swift += "\n";
                    }
                },
            };
        }

        swift
    }
}

fn generate_swift_class(
    ty: &OpaqueForeignTypeDeclaration,
    associated_funcs_and_methods: &HashMap<String, Vec<&ParsedExternFn>>,
    types: &TypeDeclarations,
    swift_bridge_path: &Path,
) -> String {
    let type_name = ty.ident.to_string();

    let mut initializers = vec![];

    let mut owned_self_methods = vec![];
    let mut ref_self_methods = vec![];
    let mut ref_mut_self_methods = vec![];

    let default_init = r#"    init() {
        fatalError("No #[swift_bridge(constructor)] was defined in the extern Rust module.")
    }"#;

    if let Some(methods) = associated_funcs_and_methods.get(&type_name) {
        for type_method in methods {
            // TODO: Normalize with freestanding func codegen above

            let func_definition = gen_func_swift_calls_rust(type_method, types, swift_bridge_path);

            let is_class_func = type_method.func.sig.inputs.is_empty();

            if type_method.is_initializer {
                initializers.push(func_definition);
            } else if is_class_func {
                ref_self_methods.push(func_definition);
            } else {
                if type_method.self_reference().is_some() {
                    if type_method.self_mutability().is_some() {
                        ref_mut_self_methods.push(func_definition);
                    } else {
                        ref_self_methods.push(func_definition);
                    }
                } else {
                    owned_self_methods.push(func_definition);
                }
            }
        }
    }

    if initializers.len() == 0 {
        initializers.push(default_init.to_string());
    }

    let initializers: String = initializers.join("\n\n");

    let mut owned_instance_methods: String = owned_self_methods.join("\n\n");
    if owned_instance_methods.len() > 0 {
        owned_instance_methods = format!("\n\n{}", owned_instance_methods);
    }

    let mut ref_instance_methods: String = ref_self_methods.join("\n\n");
    if ref_instance_methods.len() > 0 {
        ref_instance_methods = format!("\n\n{}", ref_instance_methods);
    }

    let mut ref_mut_instance_methods: String = ref_mut_self_methods.join("\n\n");
    if ref_mut_instance_methods.len() > 0 {
        ref_mut_instance_methods = format!("\n\n{}", ref_mut_instance_methods);
    }

    let free_func_call = format!("{}${}$_free(ptr)", SWIFT_BRIDGE_PREFIX, type_name);

    let class = format!(
        r#"
public class {type_name}: {type_name}RefMut {{
    var isOwned: Bool = true

{initializers}

    override init(ptr: UnsafeMutableRawPointer) {{
        super.init(ptr: ptr)
    }}

    deinit {{
        if isOwned {{
            {free_func_call}
        }}
    }}{owned_instance_methods}
}}
public class {type_name}RefMut: {type_name}Ref {{
    override init(ptr: UnsafeMutableRawPointer) {{
        super.init(ptr: ptr)
    }}{ref_mut_instance_methods}
}}
public class {type_name}Ref {{
    var ptr: UnsafeMutableRawPointer

    init(ptr: UnsafeMutableRawPointer) {{
        self.ptr = ptr
    }}{ref_instance_methods}
}}"#,
        type_name = type_name,
        initializers = initializers,
        owned_instance_methods = owned_instance_methods,
        ref_mut_instance_methods = ref_mut_instance_methods,
        ref_instance_methods = ref_instance_methods,
        free_func_call = free_func_call
    );

    return class;
}

// Generate functions to drop the reference count on a Swift class instance.
//
// # Example
//
// ```
// @_cdecl("__swift_bridge__$Foo$_free")
// func __swift_bridge__Foo__free (ptr: UnsafeMutableRawPointer) {
//     let _ = Unmanaged<Foo>.fromOpaque(ptr).takeRetainedValue()
// }
// ```
fn generate_drop_swift_instance_reference_count(ty: &OpaqueForeignTypeDeclaration) -> String {
    let link_name = ty.free_link_name();
    let fn_name = ty.free_func_name();

    format!(
        r##"
@_cdecl("{link_name}")
func {fn_name} (ptr: UnsafeMutableRawPointer) {{
    let _ = Unmanaged<{ty_name}>.fromOpaque(ptr).takeRetainedValue()
}}
"##,
        link_name = link_name,
        fn_name = fn_name,
        ty_name = ty.ty_name_ident()
    )
}

fn gen_func_swift_calls_rust(
    function: &ParsedExternFn,
    types: &TypeDeclarations,
    swift_bridge_path: &Path,
) -> String {
    let fn_name = function.sig.ident.to_string();
    let params = function.to_swift_param_names_and_types(false, types);
    let call_args = function.to_swift_call_args(true, false, types, swift_bridge_path);
    let call_fn = format!("{}({})", fn_name, call_args);

    let type_name_segment = if let Some(ty) = function.associated_type.as_ref() {
        match ty {
            TypeDeclaration::Shared(_) => {
                //
                todo!()
            }
            TypeDeclaration::Opaque(ty) => {
                format!("${}", ty.ident.to_string())
            }
        }
    } else {
        "".to_string()
    };

    let maybe_static_class_func = if function.associated_type.is_some()
        && (!function.is_method() && !function.is_initializer)
    {
        "class "
    } else {
        ""
    };

    let swift_class_func_name = if function.is_initializer {
        "init".to_string()
    } else {
        format!("func {}", fn_name.as_str())
    };

    let indentation = if function.associated_type.is_some() {
        "    "
    } else {
        ""
    };

    let call_rust = format!(
        "{prefix}{type_name_segment}${call_fn}",
        prefix = SWIFT_BRIDGE_PREFIX,
        type_name_segment = type_name_segment,
        call_fn = call_fn
    );
    let mut call_rust = if function.is_initializer {
        call_rust
    } else if let Some(built_in) = function.return_ty_built_in(types) {
        built_in.convert_ffi_value_to_swift_value(
            function.host_lang,
            TypePosition::FnReturn,
            &call_rust,
        )
    } else {
        if function.host_lang.is_swift() {
            call_rust
        } else {
            match &function.sig.output {
                ReturnType::Default => {
                    // () is a built in type so this would have been handled in the previous block.
                    unreachable!()
                }
                ReturnType::Type(_, ty) => {
                    let ty_name = match ty.deref() {
                        Type::Reference(reference) => reference.elem.to_token_stream().to_string(),
                        Type::Path(path) => path.path.segments.to_token_stream().to_string(),
                        _ => todo!(),
                    };

                    match types.get(&ty_name).unwrap() {
                        TypeDeclaration::Shared(_) => call_rust,
                        TypeDeclaration::Opaque(opaque) => {
                            if opaque.host_lang.is_rust() {
                                let (is_owned, ty) = match ty.deref() {
                                    Type::Reference(reference) => ("false", &reference.elem),
                                    _ => ("true", ty),
                                };

                                let ty = ty.to_token_stream().to_string();
                                format!("{}(ptr: {}, isOwned: {})", ty, call_rust, is_owned)
                            } else {
                                let ty = ty.to_token_stream().to_string();
                                format!(
                                    "Unmanaged<{}>.fromOpaque({}.ptr).takeRetainedValue()",
                                    ty, call_rust
                                )
                            }
                        }
                    }
                }
            }
        }
    };

    let returns_null = Some(BridgedType::StdLib(StdLibType::Null))
        == BridgedType::new_with_return_type(&function.func.sig.output, types);

    let maybe_return = if returns_null || function.is_initializer {
        ""
    } else {
        "return "
    };
    for arg in function.func.sig.inputs.iter() {
        if let Some(BridgedType::StdLib(StdLibType::Str)) = BridgedType::new_with_fn_arg(arg, types)
        {
            let arg_name = fn_arg_name(arg).unwrap().to_string();

            call_rust = format!(
                r#"{maybe_return}{arg}.utf8CString.withUnsafeBufferPointer({{{arg}Ptr in
{indentation}        {call_rust}
{indentation}    }})"#,
                maybe_return = maybe_return,
                indentation = indentation,
                arg = arg_name,
                call_rust = call_rust
            );
        }
    }

    if function.is_initializer {
        call_rust = format!("super.init(ptr: {})", call_rust)
    }

    let maybe_return = if function.is_initializer {
        "".to_string()
    } else {
        function.to_swift_return_type(types)
    };

    let func_definition = format!(
        r#"{indentation}{maybe_static_class_func}{swift_class_func_name}({params}){maybe_ret} {{
{indentation}    {call_rust}
{indentation}}}"#,
        indentation = indentation,
        maybe_static_class_func = maybe_static_class_func,
        swift_class_func_name = swift_class_func_name,
        params = params,
        maybe_ret = maybe_return,
        call_rust = call_rust
    );

    func_definition
}

fn gen_function_exposes_swift_to_rust(
    func: &ParsedExternFn,
    types: &TypeDeclarations,
    swift_bridge_path: &Path,
) -> String {
    let link_name = func.link_name();
    let prefixed_fn_name = func.prefixed_fn_name();
    let fn_name = if let Some(swift_name) = func.swift_name_override.as_ref() {
        swift_name.value()
    } else {
        func.sig.ident.to_string()
    };

    let params = func.to_swift_param_names_and_types(true, types);
    let ret = func.to_swift_return_type(types);

    let args = func.to_swift_call_args(false, true, types, swift_bridge_path);
    let mut call_fn = format!("{}({})", fn_name, args);

    if let Some(built_in) = BridgedType::new_with_return_type(&func.sig.output, types) {
        call_fn = built_in.convert_swift_expression_to_ffi_compatible(
            &call_fn,
            func.host_lang,
            swift_bridge_path,
        );

        if let Some(associated_type) = func.associated_type.as_ref() {
            let ty_name = match associated_type {
                TypeDeclaration::Shared(_) => {
                    //
                    todo!()
                }
                TypeDeclaration::Opaque(associated_type) => associated_type.ident.to_string(),
            };

            if func.is_method() {
                call_fn = format!(
                    "Unmanaged<{ty_name}>.fromOpaque(this).takeUnretainedValue().{call_fn}",
                    ty_name = ty_name,
                    call_fn = call_fn
                );
            } else if func.is_initializer {
                call_fn = format!(
                    "__private__PointerToSwiftType(ptr: Unmanaged.passRetained({}({})).toOpaque())",
                    ty_name, args
                );
            } else {
                call_fn = format!("{}::{}", ty_name, call_fn);
            }
        }
    } else {
        todo!("Push to ParsedErrors")
    };

    let generated_func = format!(
        r#"@_cdecl("{link_name}")
func {prefixed_fn_name} ({params}){ret} {{
    {call_fn}
}}
"#,
        link_name = link_name,
        prefixed_fn_name = prefixed_fn_name,
        params = params,
        ret = ret,
        call_fn = call_fn
    );

    generated_func
}

#[cfg(test)]
mod tests {
    //! TODO: We're progressively moving most of these tests to `codegen_tests.rs`,
    //!  along with their corresponding Rust token and C header generation tests.

    use crate::test_utils::assert_trimmed_generated_contains_trimmed_expected;
    use crate::SwiftBridgeModule;
    use quote::quote;
    use syn::parse_quote;

    /// Verify that we generated a Swift function to call our freestanding function.
    #[test]
    fn freestanding_rust_function_no_args() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn foo ();
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func foo() {
    __swift_bridge__$foo()
} 
"#;

        assert_eq!(generated.trim(), expected.trim());
    }

    /// Verify that we generate code to expose a freestanding Swift function.
    #[test]
    fn freestanding_swift_function_no_args() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    fn foo ();
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$foo")
func __swift_bridge__foo () {
    foo()
} 
"#;

        assert_trimmed_generated_contains_trimmed_expected(generated.trim(), expected.trim());
    }

    /// Verify that we convert slices.
    #[test]
    fn freestanding_swift_function_return_slice() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    fn foo () -> &'static [u8];
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$foo")
func __swift_bridge__foo () -> __private__FfiSlice {
    foo().toFfiSlice()
} 
"#;

        assert_trimmed_generated_contains_trimmed_expected(generated.trim(), expected.trim());
    }

    /// Verify that we convert a Swift method's returned slice.
    #[test]
    fn swift_function_return_slice() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    type MyType;
                    fn foo (&self) -> &'static [u8];
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$MyType$foo")
func __swift_bridge__MyType_foo (_ this: UnsafeMutableRawPointer) -> __private__FfiSlice {
    Unmanaged<MyType>.fromOpaque(this).takeUnretainedValue().foo().toFfiSlice()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(generated.trim(), expected.trim());
    }

    /// Verify that we generated a Swift function to call a freestanding function with one arg.
    #[test]
    fn freestanding_rust_function_one_arg() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn foo (bar: u8);
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func foo(_ bar: UInt8) {
    __swift_bridge__$foo(bar)
} 
"#;

        assert_eq!(generated.trim(), expected.trim());
    }

    /// Verify that we generated a Swift function to call a freestanding function with a return
    /// type.
    #[test]
    fn freestanding_function_with_return() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn foo () -> u32;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func foo() -> UInt32 {
    __swift_bridge__$foo()
} 
"#;

        assert_eq!(generated.trim(), expected.trim());
    }

    /// Verify that we can convert a slice reference into an UnsafeBufferPointer
    #[test]
    fn freestanding_func_return_ref_byte_slice() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn foo () -> &[u8];
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func foo() -> UnsafeBufferPointer<UInt8> {
    let slice = __swift_bridge__$foo(); return UnsafeBufferPointer(start: slice.start.assumingMemoryBound(to: UInt8.self), count: Int(slice.len));
} 
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generated a function that Rust can use to reduce a Swift class instance's
    /// reference count.
    #[test]
    fn free_class_memory() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    type Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$Foo$_free")
func __swift_bridge__Foo__free (ptr: UnsafeMutableRawPointer) {
    let _ = Unmanaged<Foo>.fromOpaque(ptr).takeRetainedValue()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generated a Swift class with an init method.
    #[test]
    fn class_with_init() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;

                    #[swift_bridge(init)]
                    fn new() -> Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
public class Foo: FooRefMut {
    var isOwned: Bool = true

    init() {
        super.init(ptr: __swift_bridge__$Foo$new())
    }

    override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$Foo$_free(ptr)
        }
    }
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we generated a function that Rust can use to reduce a Swift class instance's
    /// reference count.
    #[test]
    fn extern_swift_claas_init() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    type Foo;

                    #[swift_bridge(init)]
                    fn new (a: u8) -> Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$Foo$new")
func __swift_bridge__Foo_new (_ a: UInt8) -> __private__PointerToSwiftType {
    __private__PointerToSwiftType(ptr: Unmanaged.passRetained(Foo(a: a)).toOpaque())
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generated a Swift class with an init method with params.
    #[test]
    fn class_with_init_and_param() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;

                    #[swift_bridge(init)]
                    fn new(val: u8) -> Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
public class Foo: FooRefMut {
    var isOwned: Bool = true

    init(_ val: UInt8) {
        super.init(ptr: __swift_bridge__$Foo$new(val))
    }

    override init(ptr: UnsafeMutableRawPointer) {
        super.init(ptr: ptr)
    }

    deinit {
        if isOwned {
            __swift_bridge__$Foo$_free(ptr)
        }
    }
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we generate a Swift function that allows us to access a class instance method
    /// from Rust using a pointer.
    #[test]
    fn extern_swift_class_instance_method() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    type Foo;

                    fn push(&self, arg: u8);
                    fn pop(self: &mut Foo);
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$Foo$push")
func __swift_bridge__Foo_push (_ this: UnsafeMutableRawPointer, _ arg: UInt8) {
    Unmanaged<Foo>.fromOpaque(this).takeUnretainedValue().push(arg: arg)
}

@_cdecl("__swift_bridge__$Foo$pop")
func __swift_bridge__Foo_pop (_ this: UnsafeMutableRawPointer) {
    Unmanaged<Foo>.fromOpaque(this).takeUnretainedValue().pop()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we can generate an instance method that has a return value.
    #[test]
    fn instance_method_with_return() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;

                    fn bar(&self) -> u8;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
public class FooRef {
    var ptr: UnsafeMutableRawPointer

    init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }

    func bar() -> UInt8 {
        __swift_bridge__$Foo$bar(ptr)
    }
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we can generate a Swift instance method with an argument to a declared type.
    #[test]
    fn instance_method_with_declared_type_arg() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;

                    fn bar(self: &Foo, other: &Foo);
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
public class FooRef {
    var ptr: UnsafeMutableRawPointer

    init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }

    func bar(_ other: FooRef) {
        __swift_bridge__$Foo$bar(ptr, other.ptr)
    }
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we can generate a static class method.
    #[test]
    fn static_class_method() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    type Foo;

                    #[swift_bridge(associated_to = Foo)]
                    fn bar();
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
public class FooRef {
    var ptr: UnsafeMutableRawPointer

    init(ptr: UnsafeMutableRawPointer) {
        self.ptr = ptr
    }

    class func bar() {
        __swift_bridge__$Foo$bar()
    }
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we generate a Swift function that allows us to access a static class method
    /// from Rust using a pointer.
    #[test]
    fn extern_swift_static_class_method() {
        let tokens = quote! {
            mod foo {
                extern "Swift" {
                    type Foo;

                    #[swift_bridge(associated_to = Foo)]
                    fn bar(arg: u8);
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$Foo$bar")
func __swift_bridge__Foo_bar (_ arg: UInt8) {
    Foo::bar(arg: arg)
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, expected);
    }

    /// Verify that we properly generate a Swift function that returns a String.
    #[test]
    fn return_string() {
        let tokens = quote! {
            mod foo {
                extern "Rust" {
                    fn foo () -> String;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func foo() -> RustString {
    RustString(ptr: __swift_bridge__$foo())
}
"#;

        assert_eq!(generated.trim(), expected.trim());
    }

    /// Verify that we generate the corresponding Swift for extern "Rust" functions that accept
    /// a *const void pointer.
    #[test]
    fn extern_rust_const_void_pointer_argument() {
        let start = quote! {
            mod foo {
                extern "Rust" {
                    fn void_pointer (arg1: *const c_void);
                }
            }
        };
        let module: SwiftBridgeModule = syn::parse2(start).unwrap();
        let generated = module.generate_swift();

        let expected = r#"
func void_pointer(_ arg1: UnsafeRawPointer) {
    __swift_bridge__$void_pointer(UnsafeMutableRawPointer(mutating: arg1))
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generate the corresponding Swift for extern "Rust" functions that returns
    /// a *const void pointer.
    #[test]
    fn extern_rust_return_const_void_pointer() {
        let start = quote! {
            mod foo {
                extern "Rust" {
                    fn void_pointer () -> *const c_void;
                }
            }
        };
        let module: SwiftBridgeModule = syn::parse2(start).unwrap();
        let generated = module.generate_swift();

        let expected = r#"
func void_pointer() -> UnsafeRawPointer {
    UnsafeRawPointer(__swift_bridge__$void_pointer()!)
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generate the corresponding Swift for extern "Rust" functions that accept
    /// a *const void pointer.
    #[test]
    fn extern_swift_const_void_pointer_argument() {
        let start = quote! {
            mod foo {
                extern "Swift" {
                    fn void_pointer (arg: *const c_void);
                }
            }
        };
        let module: SwiftBridgeModule = syn::parse2(start).unwrap();
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$void_pointer")
func __swift_bridge__void_pointer (_ arg: UnsafeRawPointer) {
    void_pointer(arg: arg)
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we can return a shared struct type.
    #[test]
    fn extern_rust_return_shared_struct() {
        let tokens = quote! {
            mod ffi {
                struct Foo;

                extern "Rust" {
                    fn get_foo () -> Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func get_foo() -> Foo {
    __swift_bridge__$get_foo()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we can take a shared struct as an argument.
    #[test]
    fn extern_rust_shared_struct_arg() {
        let tokens = quote! {
            mod ffi {
                struct Foo;

                extern "Rust" {
                    fn some_function (arg: Foo);
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func some_function(_ arg: Foo) {
    __swift_bridge__$some_function(arg)
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we rename shared struct arguments and return values if there is a swift_name
    /// attribute.
    #[test]
    fn extern_rust_fn_uses_swift_name_for_shared_struct_attrib() {
        let tokens = quote! {
            mod ffi {
                #[swift_bridge(swift_name = "Renamed")]
                struct Foo;

                extern "Rust" {
                    fn some_function (arg: Foo) -> Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func some_function(_ arg: Renamed) -> Renamed {
    __swift_bridge__$some_function(arg)
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generate the correct function for an extern "Rust" fn that takes an owned
    /// opaque Swift type.
    #[test]
    fn extern_rust_fn_with_extern_swift_owned_opaque_arg() {
        let tokens = quote! {
            mod ffi {
                extern "Rust" {
                    fn some_function (arg: Foo);
                }

                extern "Swift" {
                    type Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func some_function(_ arg: Foo) {
    __swift_bridge__$some_function(__private__PointerToSwiftType(ptr: Unmanaged.passRetained(arg).toOpaque()))
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we generate the correct function for an extern "Rust" fn that returns an owned
    /// opaque Swift type.
    #[test]
    fn extern_rust_fn_returns_extern_swift_owned_opaque_type() {
        let tokens = quote! {
            mod ffi {
                extern "Rust" {
                    fn some_function () -> Foo;
                }

                extern "Swift" {
                    type Foo;
                }
            }
        };
        let module: SwiftBridgeModule = parse_quote!(#tokens);
        let generated = module.generate_swift();

        let expected = r#"
func some_function() -> Foo {
    Unmanaged<Foo>.fromOpaque(__swift_bridge__$some_function().ptr).takeRetainedValue()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }

    /// Verify that we use a function's `swift_name = "..."` attribute during Swift codegen for
    /// extern Swift functions.
    #[test]
    fn extern_swift_uses_swift_name_function_attribute() {
        let start = quote! {
            mod foo {
                extern "Swift" {
                    #[swift_bridge(swift_name = "someFunctionSwiftName")]
                    fn some_function ();
                }
            }
        };
        let module: SwiftBridgeModule = syn::parse2(start).unwrap();
        let generated = module.generate_swift();

        let expected = r#"
@_cdecl("__swift_bridge__$some_function")
func __swift_bridge__some_function () {
    someFunctionSwiftName()
}
"#;

        assert_trimmed_generated_contains_trimmed_expected(&generated, &expected);
    }
}
