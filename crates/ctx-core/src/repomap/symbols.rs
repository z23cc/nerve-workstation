use crate::codemap::CodeSymbol;

const MAX_SYMBOLS_PER_FILE: usize = 12;

pub(super) fn key_symbols(symbols: &[CodeSymbol]) -> Vec<CodeSymbol> {
    symbols.iter().take(MAX_SYMBOLS_PER_FILE).cloned().collect()
}
