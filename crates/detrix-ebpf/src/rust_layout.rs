//! Bounded Rust ABI/layout contracts used by the eBPF decoder.
//!
//! These contracts intentionally mirror the facts a debugger formatter uses,
//! without evaluating target code or chasing arbitrary heap graphs.  A Rust
//! trait object is represented by two addresses (data, vtable); common niche
//! `Option` values are represented by a nullable pointer word.  Async/generator
//! state is only accepted when the compiler exposes an explicit byte-sized
//! state field; otherwise callers must keep the value opaque/unavailable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustLayoutContract {
    NicheOption {
        pointer_offset: usize,
        word_size: usize,
    },
    TraitObject {
        data_offset: usize,
        vtable_offset: usize,
        word_size: usize,
    },
    AsyncState {
        state_offset: usize,
        state_size: usize,
    },
}

/// Infer only layouts whose ABI is stable enough for bounded address/state
/// capture.  The function is deliberately conservative: a name match alone
/// never authorizes dereferencing or recursive traversal.
pub fn infer(name: &str, byte_size: usize) -> Option<RustLayoutContract> {
    let normalized = name.trim();
    if (normalized.starts_with("Option<")
        || normalized.starts_with("core::option::Option<")
        || normalized.starts_with("std::option::Option<"))
        && (byte_size == 8 || byte_size == 16)
    {
        let inner = normalized
            .split_once('<')
            .and_then(|(_, rest)| rest.strip_suffix('>'))
            .unwrap_or_default();
        let pointer_like = inner.starts_with('&')
            || inner.starts_with("*const ")
            || inner.starts_with("*mut ")
            || inner.starts_with("Box<")
            || inner.starts_with("alloc::boxed::Box<")
            || inner.starts_with("NonNull<")
            || inner.starts_with("core::ptr::NonNull<")
            || inner.starts_with("Arc<")
            || inner.starts_with("Rc<");
        if pointer_like {
            return Some(RustLayoutContract::NicheOption {
                pointer_offset: 0,
                word_size: 8,
            });
        }
    }

    if normalized.contains("dyn ") && byte_size == 16 {
        return Some(RustLayoutContract::TraitObject {
            data_offset: 0,
            vtable_offset: 8,
            word_size: 8,
        });
    }

    // This is intentionally opt-in by an explicit compiler/debug name.  A
    // generic future/generator's state byte is not inferable from its name;
    // callers must use a DWARF-proven field or retain an opaque blob.
    if (normalized.contains("DetrixAsyncState") || normalized.contains("detrix::AsyncState"))
        && byte_size >= 1
    {
        return Some(RustLayoutContract::AsyncState {
            state_offset: 0,
            state_size: 1,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_pointer_niche_option_is_bounded() {
        assert_eq!(
            infer("core::option::Option<&Foo>", 8),
            Some(RustLayoutContract::NicheOption {
                pointer_offset: 0,
                word_size: 8
            })
        );
        assert!(infer("Option<u64>", 8).is_none());
    }

    #[test]
    fn trait_object_is_two_addresses() {
        assert_eq!(
            infer("&dyn Display", 16),
            Some(RustLayoutContract::TraitObject {
                data_offset: 0,
                vtable_offset: 8,
                word_size: 8
            })
        );
    }

    #[test]
    fn async_state_requires_explicit_contract_name() {
        assert!(matches!(
            infer("detrix::AsyncState", 8),
            Some(RustLayoutContract::AsyncState { .. })
        ));
        assert!(infer("core::future::GenFuture", 8).is_none());
    }
}
