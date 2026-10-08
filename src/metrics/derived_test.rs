use super::derived::*;

// --- memory_used_percent ---

#[test]
fn test_memory_used_percent_typical() {
    let pct = memory_used_percent(16.0e9, 4.0e9).unwrap();
    assert!((pct - 75.0).abs() < 1e-9);
}

#[test]
fn test_memory_used_percent_zero_total_is_none() {
    assert_eq!(memory_used_percent(0.0, 0.0), None);
}

#[test]
fn test_memory_used_percent_negative_or_nan_total_is_none() {
    assert_eq!(memory_used_percent(-1.0, 0.0), None);
    assert_eq!(memory_used_percent(f64::NAN, 0.0), None);
    assert_eq!(memory_used_percent(f64::INFINITY, 0.0), None);
}

#[test]
fn test_memory_used_percent_available_exceeds_total_clamps_to_zero() {
    assert_eq!(memory_used_percent(8.0e9, 9.0e9), Some(0.0));
}

#[test]
fn test_memory_used_percent_nothing_available_is_hundred() {
    assert_eq!(memory_used_percent(8.0e9, 0.0), Some(100.0));
}

// --- swap_used_percent ---

#[test]
fn test_swap_used_percent_typical() {
    let pct = swap_used_percent(8.0e9, 2.0e9).unwrap();
    assert!((pct - 25.0).abs() < 1e-9);
}

#[test]
fn test_swap_used_percent_no_swap_is_none() {
    assert_eq!(swap_used_percent(0.0, 0.0), None);
}

#[test]
fn test_swap_used_percent_unused_swap_is_zero() {
    assert_eq!(swap_used_percent(8.0e9, 0.0), Some(0.0));
}

// --- cpu_busy_percent ---

#[test]
fn test_cpu_busy_percent_complements_idle() {
    assert_eq!(cpu_busy_percent(87.5), Some(12.5));
    assert_eq!(cpu_busy_percent(100.0), Some(0.0));
    assert_eq!(cpu_busy_percent(0.0), Some(100.0));
}

#[test]
fn test_cpu_busy_percent_out_of_range_idle_clamps() {
    assert_eq!(cpu_busy_percent(100.000_001), Some(0.0));
    assert_eq!(cpu_busy_percent(-0.5), Some(100.0));
}

#[test]
fn test_cpu_busy_percent_nan_is_none() {
    assert_eq!(cpu_busy_percent(f64::NAN), None);
}

// --- load_per_cpu ---

#[test]
fn test_load_per_cpu_divides_by_count() {
    assert_eq!(load_per_cpu(6.0, 4), Some(1.5));
    assert_eq!(load_per_cpu(0.0, 8), Some(0.0));
}

#[test]
fn test_load_per_cpu_zero_cpus_is_none() {
    assert_eq!(load_per_cpu(1.0, 0), None);
}

#[test]
fn test_load_per_cpu_nan_load_is_none() {
    assert_eq!(load_per_cpu(f64::NAN, 4), None);
}

// --- count_cpu_list ---

#[test]
fn test_count_cpu_list_single_range() {
    assert_eq!(count_cpu_list("0-7\n"), Some(8));
}

#[test]
fn test_count_cpu_list_single_cpu() {
    assert_eq!(count_cpu_list("0"), Some(1));
}

#[test]
fn test_count_cpu_list_mixed_ranges_and_singletons() {
    assert_eq!(count_cpu_list("0-3,5,8-11"), Some(9));
}

#[test]
fn test_count_cpu_list_empty_is_none() {
    assert_eq!(count_cpu_list(""), None);
    assert_eq!(count_cpu_list("  \n"), None);
}

#[test]
fn test_count_cpu_list_malformed_is_none() {
    assert_eq!(count_cpu_list("0-x"), None);
    assert_eq!(count_cpu_list("a"), None);
    assert_eq!(count_cpu_list("0,,2"), None);
}

#[test]
fn test_count_cpu_list_reversed_range_is_none() {
    assert_eq!(count_cpu_list("7-3"), None);
}
