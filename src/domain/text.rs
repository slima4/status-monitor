/// Renders as nothing, so it can hide an instruction inside text a human and a
/// model read differently. `char::is_control` covers only the C0/C1 block.
pub fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                  // soft hyphen
        | '\u{061C}'                // arabic letter mark
        | '\u{200B}'..='\u{200F}'   // zero-width, bidi marks
        | '\u{202A}'..='\u{202E}'   // bidi embedding and override
        | '\u{2060}'..='\u{2064}'   // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}'   // bidi isolates
        | '\u{FEFF}'                // zero-width no-break space
        | '\u{E0000}'..='\u{E007F}' // tag characters
    )
}
