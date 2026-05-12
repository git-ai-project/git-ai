use crate::error::ErrorsVec;
use crate::parse::arguments::TestAttr;
use crate::parse::just_once::{AttrBuilder, JustOnceFnAttributeExtractor, Validator};
use crate::utils::attr_ends_with;
use quote::quote;
use syn::visit_mut::VisitMut;
use syn::{parse2, parse_quote, Attribute, ItemFn};

pub(crate) fn extract_test_attr(item_fn: &mut ItemFn) -> Result<Option<TestAttr>, ErrorsVec> {
    let mut extractor = JustOnceFnAttributeExtractor::<TestAttrBuilder>::new("test_attr");

    extractor.visit_item_fn_mut(item_fn);
    extractor.take().map(|extracted| match extracted {
        Some(a) => Some(a.into()),
        None => {
            if first_valid_test_attrs(item_fn.attrs.as_slice()).is_some() {
                Some(TestAttr::InAttrs)
            } else {
                None
            }
        }
    })
}

pub fn first_valid_test_attrs(attributes: &[Attribute]) -> Option<&Attribute> {
    attributes
        .iter()
        .find(|attr| attr_ends_with(attr, &parse_quote! { test }))
}

struct TestAttrBuilder;

impl AttrBuilder<ItemFn> for TestAttrBuilder {
    type Out = syn::Attribute;

    fn build(attr: syn::Attribute, _ident: &ItemFn) -> syn::Result<Self::Out> {
        match &attr.meta {
            syn::Meta::List(meta_list) => {
                let tokens = &meta_list.tokens;
                let item_fn: Option<ItemFn> = parse2(quote! {#[#tokens] fn ____f() {}}).ok();
                item_fn.and_then(|mut f| f.attrs.pop())
            }
            _ => None,
        }
        .ok_or_else(|| {
            syn::Error::new_spanned(
                attr,
                "invalid `test_attr` syntax; should be `#[test_attr(<test attribute>)]`",
            )
        })
    }
}
impl Validator<ItemFn> for TestAttrBuilder {
    fn validate(item_fn: &ItemFn) -> syn::Result<()> {
        match first_valid_test_attrs(item_fn.attrs.as_slice()) {
            None => Ok(()),
            Some(t) => Err(syn::Error::new_spanned(
                t,
                "You cannot use both explicit `test_attr` and a valid test attribute",
            )),
        }
    }
}

#[cfg(test)]
mod should {
    use super::*;
    use crate::test::{assert_eq, *};

    use crate::test::{attr, ToAst};

    fn explicit(a: &str) -> TestAttr {
        TestAttr::Explicit(attr(format!("#[{a}]")).into())
    }

    fn in_attrs() -> TestAttr {
        TestAttr::InAttrs
    }

    #[rstest]
    #[case::base_explicit(
        "#[test_attr(great_attr_test)]fn test(){}",
        "fn test(){}",
        Some(explicit("great_attr_test"))
    )]
    #[case::base_in_attr("#[test] fn test(){}", "#[test] fn test(){}", Some(in_attrs()))]
    #[case::base_no_one("#[a] #[b] #[c] fn test(){}", "#[a] #[b] #[c] fn test(){}", None)]
    #[case::explicit_with_others(
        "#[a] #[test_attr(great_attr_test)] #[b] #[c] fn test(){}",
        "#[a] #[b] #[c] fn test(){}",
        Some(explicit("great_attr_test"))
    )]
    #[case::in_attr_with_others(
        "#[aa] #[bb] #[test] #[cc] fn test(){}",
        "#[aa] #[bb] #[test] #[cc] fn test(){}",
        Some(in_attrs())
    )]
    #[case::in_attr_segment_path(
        "#[aa] #[bb] #[a::b::c::test] #[cc] fn test(){}",
        "#[aa] #[bb] #[a::b::c::test] #[cc] fn test(){}",
        Some(in_attrs())
    )]
    #[case::structured_explicit(
        "#[test_attr(macro_rules_attribute::apply(smol_macros::test!))] fn test(){}",
        "fn test(){}",
        Some(explicit("macro_rules_attribute::apply(smol_macros::test!)"))
    )]
    #[case::in_attr_not_guessing(
        "#[a_test] fn test(){}",
        "#[a_test] fn test(){}",
        None
    )]
    #[case::in_attr_not_guessing(
        "#[test_a] fn test(){}",
        "#[test_a] fn test(){}",
        None
    )]
    fn extract(
        #[case] item_fn: &str,
        #[case] expected_fn: &str,
        #[case] extracted: Option<TestAttr>,
    ) {
        let mut item_fn: ItemFn = item_fn.ast();
        let expected_fn: ItemFn = expected_fn.ast();

        let test_attr = extract_test_attr(&mut item_fn).unwrap();

        assert_eq!(expected_fn, item_fn);
        assert_eq!(extracted, test_attr)
    }

    mod raise_error_if {
        use super::*;

        #[test]
        #[should_panic(expected = "more than once")]
        fn explicit_attribute_is_present_more_than_one() {
            let mut item_fn: ItemFn = r#"
                #[aa]
                #[test_attr(great_attr_test)]
                #[bb] 
                #[test_attr(other)]
                #[c] 
                fn test(){}"#
                .ast();
            extract_test_attr(&mut item_fn).unwrap();
        }

        #[test]
        #[should_panic(expected = "cannot use both")]
        fn have_both_explicit_and_in_attrs() {
            let mut item_fn: ItemFn = r#"
                #[test_attr(great_attr_test)]
                #[my::great::test]
                fn test(){}"#
                .ast();
            extract_test_attr(&mut item_fn).unwrap();
        }

        #[rstest]
        #[case::no_value("#[test_attr]")]
        #[case::name_value(r#"#[test_attr = "some"]"#)]
        #[case::invalid_list(r#"#[test_attr(a,b)]"#)]
        #[should_panic(expected = "syntax")]
        fn the_test_attr_has_wrong_syntax(#[case] attr: &str) {
            let mut item_fn: ItemFn = format!(
                r#"
                {attr}
                fn test(){{}}"#
            )
            .ast();

            extract_test_attr(&mut item_fn).unwrap();
        }
    }
}
