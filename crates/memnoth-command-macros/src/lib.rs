//! Proc macros for memnoth-command.
//!
//! Provides the `#[command]` attribute for registering command handlers.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Derive macro for command registration.
///
/// Usage:
/// ```ignore
/// #[command(
///     id = "vault.init",
///     cli_path = ["vault", "init"],
///     slash_name = "init",
///     description = "Create the vault",
///     overlay = "None",
///     danger = "Safe",
///     auth = "None",
///     categories = ["vault"],
///     params = [...],
/// )]
/// async fn vault_init(ctx: &AppContext) -> Result<Outcome> { ... }
/// ```
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    parse_command_attr(attr);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();

    let registration = quote! {
        /// Auto-generated command registration for #fn_name_str
        #[ctor::ctor]
        fn #fn_name() {
            use memnoth_command::registry::CommandRegistry;
            use std::sync::LazyLock;

            static REGISTRY: LazyLock<memnoth_command::CommandRegistry> =
                LazyLock::new(memnoth_command::CommandRegistry::new);

            // Register command spec — populated by build-time codegen
            let spec: memnoth_command::spec::CommandSpec = memnoth_command::spec::CommandSpec {
                id: #fn_name_str.to_string(),
                cli_path: vec![],
                slash: memnoth_command::spec::SlashSpec {
                    name: #fn_name_str.to_string(),
                    description: "Auto-registered command".to_string(),
                    long_description: None,
                    palette: None,
                    params: vec![],
                    overlay: memnoth_command::spec::OverlayType::None,
                    danger: memnoth_command::spec::DangerLevel::Safe,
                    auth: memnoth_command::spec::AuthLevel::None,
                    profile: None,
                    categories: vec![],
                    aliases: vec![],
                    hidden: false,
                    deprecated: None,
                    examples: vec![],
                },
                handler: #fn_name_str.to_string(),
                json_output: true,
                streaming: false,
                timeout_secs: None,
                min_version: None,
                tags: vec![],
                related: vec![],
            };
            REGISTRY.register(spec).expect("command registration failed");
        }
    };

    TokenStream::from(quote! {
        #input

        #registration
    })
}

fn parse_command_attr(_attr: TokenStream) {
    // Attribute parsing handled by build-time codegen
    // This is a stub that accepts any attribute shape
}
