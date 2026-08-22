//! Derivation of same-service Fastly edge candidates in other regions.
//!
//! Empirical pattern from mosdns discussion #511: segments sharing the same
//! "type" (third-octet % 64 class) serve the same domains with an unchanged
//! fourth octet.

use std::net::Ipv4Addr;

/// Known regional segments: (first, second, third octet, type).
const SEGMENTS: &[(u8, u8, u8, u8)] = &[
    // Type 1
    (151, 101, 0, 1),
    (151, 101, 64, 1),
    (151, 101, 128, 1),
    (151, 101, 192, 1),
    (151, 101, 108, 1), // Tokyo
    (146, 75, 112, 1),  // Tokyo
    (151, 101, 40, 1),  // San Jose
    (199, 232, 192, 1), // San Jose
    (151, 101, 88, 1),  // Osaka
    // Type 2
    (151, 101, 1, 2),
    (151, 101, 65, 2),
    (151, 101, 129, 2),
    (151, 101, 193, 2),
    (151, 101, 109, 2), // Tokyo
    (146, 75, 113, 2),  // Tokyo
    (151, 101, 41, 2),  // San Jose
    (199, 232, 193, 2), // San Jose
    (151, 101, 89, 2),  // Osaka
    (146, 75, 93, 2),   // Los Angeles
    // Type 3
    (151, 101, 2, 3),
    (151, 101, 66, 3),
    (151, 101, 130, 3),
    (151, 101, 194, 3),
    (151, 101, 110, 3), // Tokyo
    (146, 75, 114, 3),  // Tokyo
    (151, 101, 42, 3),  // San Jose
    (199, 232, 194, 3), // San Jose
    (151, 101, 90, 3),  // Osaka
    (151, 101, 26, 3),  // Los Angeles
    // Type 4
    (151, 101, 3, 4),
    (151, 101, 67, 4),
    (151, 101, 131, 4),
    (151, 101, 195, 4),
    (151, 101, 111, 4), // Tokyo
    (146, 75, 115, 4),  // Tokyo
    (151, 101, 43, 4),  // San Jose
    (199, 232, 195, 4), // San Jose
    (151, 101, 91, 4),  // Osaka
    (151, 101, 27, 4),  // Los Angeles
];

/// Returns same-type segments in other regions with the fourth octet kept,
/// excluding the input itself. Empty if `ip` belongs to no known segment.
pub fn derive_region_candidates(ip: &Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, d] = ip.octets();
    let Some(&(_, _, _, kind)) = SEGMENTS
        .iter()
        .find(|&&(sa, sb, sc, _)| (sa, sb, sc) == (a, b, c))
    else {
        return Vec::new();
    };
    SEGMENTS
        .iter()
        .filter(|&&(sa, sb, sc, k)| k == kind && (sa, sb, sc) != (a, b, c))
        .map(|&(sa, sb, sc, _)| Ipv4Addr::new(sa, sb, sc, d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn anycast_type2_derives_all_other_type2_segments() {
        let mut derived: Vec<_> = derive_region_candidates(&Ipv4Addr::new(151, 101, 1, 91));
        derived.sort();
        let expected: BTreeSet<_> = [
            Ipv4Addr::new(151, 101, 65, 91),
            Ipv4Addr::new(151, 101, 129, 91),
            Ipv4Addr::new(151, 101, 193, 91),
            Ipv4Addr::new(151, 101, 109, 91),
            Ipv4Addr::new(146, 75, 113, 91),
            Ipv4Addr::new(151, 101, 41, 91),
            Ipv4Addr::new(199, 232, 193, 91),
            Ipv4Addr::new(151, 101, 89, 91),
            Ipv4Addr::new(146, 75, 93, 91),
        ]
        .into_iter()
        .collect();
        assert_eq!(derived.iter().copied().collect::<BTreeSet<_>>(), expected);
        assert!(derived.iter().all(|ip| ip.octets()[3] == 91));
    }

    #[test]
    fn tokyo_type2_input_reverse_derives_anycast() {
        let derived = derive_region_candidates(&Ipv4Addr::new(146, 75, 113, 91));
        assert!(derived.contains(&Ipv4Addr::new(151, 101, 1, 91)));
    }

    #[test]
    fn type1_input_derives_type1_only() {
        let derived = derive_region_candidates(&Ipv4Addr::new(151, 101, 0, 77));
        assert!(derived.contains(&Ipv4Addr::new(151, 101, 108, 77)));
        assert!(derived.contains(&Ipv4Addr::new(199, 232, 192, 77)));
        // No type-2 segments leak in.
        assert!(!derived.contains(&Ipv4Addr::new(151, 101, 1, 77)));
        assert!(!derived.contains(&Ipv4Addr::new(146, 75, 93, 77)));
        assert!(derived.iter().all(|ip| ip.octets()[3] == 77));
    }

    #[test]
    fn unknown_segment_returns_empty() {
        assert!(derive_region_candidates(&Ipv4Addr::new(8, 8, 8, 8)).is_empty());
    }
}
