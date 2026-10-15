//! Port of upstream/apps/server/src/git/detachStackFrame.ts
//!
//! Effect's `Cache.get` retained the caller's request stack for the cache TTL.
//! Turin has no Effect fiber stack, so this is the identity: it returns the
//! value it was given. Call sites stay so a host can see where a lookup was
//! detached from its caller.

/// Documented no-op identity for the Effect cache stack-frame workaround.
pub fn detach_stack_frame<T>(value: T) -> T {
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_value() {
        assert_eq!(detach_stack_frame(41), 41);
        assert_eq!(detach_stack_frame("lookup"), "lookup");
    }
}
