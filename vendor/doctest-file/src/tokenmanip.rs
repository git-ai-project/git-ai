use super::*;

pub(crate) fn macro_invoc(mac: I, ts: TS) -> TS {
    let mut exclaim = Punct::new('!', Spacing::Joint);
    exclaim.set_span(mac.span());
    let mut parens = Group::new(Delimiter::Parenthesis, ts);
    parens.set_span(mac.span());
    [TT::Ident(mac), TT::Punct(exclaim), TT::Group(parens)].into_iter().collect::<TS>()
}

pub(crate) fn compile_error(error: Error) -> TS {
    let mut inparens = TS::new();
    let mut msg = Literal::string(&error.msg);
    msg.set_span(error.span);
    inparens.extend([TT::Literal(msg)]);
    macro_invoc(I::new("compile_error", error.span), inparens)
}

pub(crate) fn parse_literal(lit: &Literal) -> MResult<String> {
    let span = lit.span();
    let tx = lit.to_string();
    let notasl = || err("not a string literal", span);
    let Some(s) = tx.strip_suffix('"') else {
        return notasl();
    };
    let error_on_escapes;
    let s = if let Some(s) = s.strip_prefix('r') {
        error_on_escapes = false;
        s
    } else {
        error_on_escapes = true;
        s
    };
    let Some(s) = s.strip_prefix('"') else {
        return notasl();
    };

    if error_on_escapes {
        for c in s.chars() {
            if c == '\\' {
                return err("escape sequences are unsupported", span);
            }
        }
    }
    Ok(s.to_owned())
}
