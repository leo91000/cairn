//! Human-readable aliases. UUIDs remain the internal identity and authorization keys.
const ADJECTIVES: [&str; 16] = [
    "bright", "calm", "clear", "gentle", "happy", "little", "lucky", "merry", "quiet", "silver",
    "soft", "sunny", "swift", "warm", "wild", "wise",
];

const NOUNS: [&str; 16] = [
    "birch", "brook", "cedar", "comet", "coral", "dawn", "fern", "finch", "fox", "maple", "moon",
    "otter", "pine", "robin", "star", "willow",
];

/// Call with fresh random UUID bytes; storage enforces uniqueness and retries collisions.
pub fn readable(bytes: &[u8; 16]) -> String {
    let adjective = ADJECTIVES[usize::from(bytes[0] & 15)];
    let noun = NOUNS[usize::from(bytes[1] & 15)];
    let suffix = u32::from_be_bytes(bytes[12..16].try_into().expect("four bytes"));

    format!("{adjective}-{noun}-{suffix:08x}")
}
