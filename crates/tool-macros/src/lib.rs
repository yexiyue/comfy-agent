use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{Expr, FnArg, ItemFn, Lit, LitStr, Meta, ReturnType, Type, parse::Parser};

/// Generates a PascalCase `FunctionNameTool`, preserving the original async function.
/// Accepts `async fn(args: Args) -> Result<Output>` or an additional `ctx: &Context`.
#[proc_macro_attribute]
pub fn agent_tool(attributes: TokenStream, item: TokenStream) -> TokenStream {
    expand(attributes.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(
    attributes: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let function: ItemFn = syn::parse2(item)?;
    let signature = &function.sig;
    if signature.asyncness.is_none()
        || signature.constness.is_some()
        || signature.unsafety.is_some()
        || signature.abi.is_some()
        || signature.variadic.is_some()
        || !signature.generics.params.is_empty()
        || signature.generics.where_clause.is_some()
    {
        return Err(syn::Error::new_spanned(
            signature,
            "agent_tool requires a safe, non-generic async function",
        ));
    }
    if !(1..=2).contains(&signature.inputs.len()) {
        return Err(syn::Error::new_spanned(
            &signature.inputs,
            "expected one owned argument and an optional &Context",
        ));
    }
    let first = match &signature.inputs[0] {
        FnArg::Typed(argument) if !matches!(*argument.ty, Type::Reference(_)) => &argument.ty,
        argument => {
            return Err(syn::Error::new_spanned(
                argument,
                "first argument must be an owned parameters type, not self or a reference",
            ));
        }
    };
    let context = if signature.inputs.len() == 2 {
        match &signature.inputs[1] {
            FnArg::Typed(argument) => match argument.ty.as_ref() {
                Type::Reference(reference)
                    if reference.mutability.is_none() && reference.lifetime.is_none() =>
                {
                    Some(&reference.elem)
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        argument,
                        "context must be a shared reference: ctx: &Context",
                    ));
                }
            },
            argument => return Err(syn::Error::new_spanned(argument, "context cannot be self")),
        }
    } else {
        None
    };
    let is_result = match &signature.output {
        ReturnType::Type(_, ty) => match ty.as_ref() {
            Type::Path(path) => path
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Result"),
            _ => false,
        },
        _ => false,
    };
    if !is_result {
        return Err(syn::Error::new_spanned(
            &signature.output,
            "return type must be Result<T> or Result<T, E>, with T: Serialize",
        ));
    }

    let mut name = None;
    let mut description = None;
    let options =
        syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated.parse2(attributes)?;
    for option in options {
        let Meta::NameValue(value) = &option else {
            return Err(syn::Error::new_spanned(
                option,
                "expected name = \"...\" or description = \"...\"",
            ));
        };
        let Expr::Lit(literal) = &value.value else {
            return Err(syn::Error::new_spanned(
                &value.value,
                "expected a string literal",
            ));
        };
        let Lit::Str(text) = &literal.lit else {
            return Err(syn::Error::new_spanned(
                &value.value,
                "expected a string literal",
            ));
        };
        let target = if value.path.is_ident("name") {
            &mut name
        } else if value.path.is_ident("description") {
            &mut description
        } else {
            return Err(syn::Error::new_spanned(
                &value.path,
                "unknown option; use name or description",
            ));
        };
        if target.replace(text.clone()).is_some() {
            return Err(syn::Error::new_spanned(option, "duplicate option"));
        }
    }
    let identifier = &signature.ident;
    let function_name = identifier.to_string();
    let function_name = function_name.trim_start_matches("r#");
    let name = name.unwrap_or_else(|| LitStr::new(function_name, identifier.span()));
    let name_value = name.value();
    if name_value.is_empty()
        || name_value.len() > 64
        || !name_value
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        || !name_value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(syn::Error::new_spanned(
            name,
            "tool name must start with an ASCII letter or _, contain only letters, digits, _ or -, and be at most 64 bytes",
        ));
    }
    let description = description.unwrap_or_else(|| {
        let docs: Vec<String> = function
            .attrs
            .iter()
            .filter_map(|attribute| {
                if !attribute.path().is_ident("doc") {
                    return None;
                }
                match &attribute.meta {
                    Meta::NameValue(value) => match &value.value {
                        Expr::Lit(literal) => match &literal.lit {
                            Lit::Str(text) => Some(text.value().trim().to_owned()),
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                }
            })
            .collect();
        LitStr::new(docs.join("\n").trim(), identifier.span())
    });
    if description.value().trim().is_empty() {
        return Err(syn::Error::new_spanned(
            identifier,
            "provide doc comments or a nonempty description",
        ));
    }

    let runtime = match proc_macro_crate::crate_name("tools") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(::tools),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote!(::#ident)
        }
        Err(error) => {
            return Err(syn::Error::new(
                Span::call_site(),
                format!("add a dependency on tools: {error}"),
            ));
        }
    };
    let pascal: String = function_name
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect();
    let wrapper = format_ident!("{}Tool", pascal, span = identifier.span());
    let visibility = &function.vis;
    let cfg = function
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"));
    let cfg: Vec<_> = cfg.collect();
    let (structure, call) = if let Some(context) = context {
        (
            quote! {
                #(#cfg)*
                #visibility struct #wrapper { context: ::std::sync::Arc<#context> }
                #(#cfg)*
                impl #wrapper {
                    #visibility fn new(context: ::std::sync::Arc<#context>) -> Self { Self { context } }
                }
            },
            quote!(__agent_tool_function(args, self.context.as_ref()).await?),
        )
    } else {
        (
            quote! { #(#cfg)* #visibility struct #wrapper; },
            quote!(__agent_tool_function(args).await?),
        )
    };
    Ok(quote! {
        #function
        #structure
        #(#cfg)*
        impl #runtime::AgentTool for #wrapper {
            fn name(&self) -> &'static str { #name }
            fn description(&self) -> &'static str { #description }
            fn schema(&self) -> #runtime::__private::serde_json::Value {
                #runtime::__private::schemars::schema_for!(#first).to_value()
            }
            fn execute(&self, arguments: #runtime::__private::serde_json::Value)
                -> #runtime::__private::futures::future::BoxFuture<'_, #runtime::__private::anyhow::Result<#runtime::__private::serde_json::Value>>
            {
                ::std::boxed::Box::pin(async move {
                    let __agent_tool_function = #identifier;
                    let args: #first = #runtime::__private::serde_json::from_value(arguments)
                        .map_err(|error| #runtime::__private::anyhow::anyhow!("invalid arguments for {}: {}", #name, error))?;
                    let result = #call;
                    Ok(#runtime::__private::serde_json::to_value(result)?)
                })
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::expand;
    use quote::quote;

    #[test]
    fn rejects_unsupported_signatures_and_options() {
        let cases = [
            (
                quote!(),
                quote!(
                    fn tool(args: Args) -> Result<Value> {}
                ),
                "non-generic async",
            ),
            (
                quote!(),
                quote!(
                    async fn tool<T>(args: T) -> Result<Value> {}
                ),
                "non-generic async",
            ),
            (
                quote!(),
                quote!(
                    async fn tool() -> Result<Value> {}
                ),
                "one owned argument",
            ),
            (
                quote!(),
                quote!(
                    async fn tool(args: &Args) -> Result<Value> {}
                ),
                "owned parameters",
            ),
            (
                quote!(),
                quote!(
                    async fn tool(args: Args, ctx: &mut Context) -> Result<Value> {}
                ),
                "shared reference",
            ),
            (
                quote!(),
                quote!(
                    async fn tool(args: Args) -> Value {}
                ),
                "return type must be Result",
            ),
            (
                quote!(unknown = "x"),
                quote!(
                    async fn tool(args: Args) -> Result<Value> {}
                ),
                "unknown option",
            ),
            (
                quote!(name = "x", name = "y"),
                quote!(
                    async fn tool(args: Args) -> Result<Value> {}
                ),
                "duplicate option",
            ),
            (
                quote!(name = 42),
                quote!(
                    async fn tool(args: Args) -> Result<Value> {}
                ),
                "string literal",
            ),
            (
                quote!(name = "bad name"),
                quote!(
                    async fn tool(args: Args) -> Result<Value> {}
                ),
                "tool name must",
            ),
            (
                quote!(),
                quote!(
                    async fn tool(args: Args) -> Result<Value> {}
                ),
                "provide doc comments",
            ),
        ];
        for (attributes, item, expected) in cases {
            let error = expand(attributes, item).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error:?}"
            );
        }
    }
}
