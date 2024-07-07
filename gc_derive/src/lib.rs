use proc_macro::TokenStream;
use quote::quote;

#[proc_macro_derive(Trace, attributes(trace))]
pub fn derive_trace(item: TokenStream) -> TokenStream {
    TokenStream::new()
}
