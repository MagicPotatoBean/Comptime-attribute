use comptime_impl::comptime_impl;
mod comptime_impl;
extern crate proc_macro;
use proc_macro::TokenStream;
#[proc_macro_attribute]
pub fn comptime(args: TokenStream, item: TokenStream) -> TokenStream {
    comptime_impl(args, item)
}