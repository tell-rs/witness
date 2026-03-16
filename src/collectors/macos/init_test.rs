use crate::config::SystemConfig;

#[test]
fn init_all_collectors() {
    let config = SystemConfig::default();
    let collectors = super::init_collectors(&config);
    let names: Vec<&str> = collectors.iter().map(|c| c.name()).collect();
    assert!(names.contains(&"load"));
    assert!(names.contains(&"memory"));
    assert!(names.contains(&"cpu"));
    assert!(names.contains(&"disk"));
    assert!(names.contains(&"network"));
    assert_eq!(collectors.len(), 5);
}

#[test]
fn init_no_collectors() {
    let config = SystemConfig {
        cpu: false,
        memory: false,
        load: false,
        disk: false,
        network: false,
        ..SystemConfig::default()
    };
    let collectors = super::init_collectors(&config);
    assert!(collectors.is_empty());
}

#[test]
fn init_subset() {
    let config = SystemConfig {
        cpu: true,
        memory: true,
        load: false,
        disk: false,
        network: false,
        ..SystemConfig::default()
    };
    let collectors = super::init_collectors(&config);
    let names: Vec<&str> = collectors.iter().map(|c| c.name()).collect();
    assert!(names.contains(&"cpu"));
    assert!(names.contains(&"memory"));
    assert!(!names.contains(&"load"));
    assert!(!names.contains(&"disk"));
    assert!(!names.contains(&"network"));
    assert_eq!(collectors.len(), 2);
}

#[test]
fn init_single_collector() {
    let config = SystemConfig {
        cpu: false,
        memory: false,
        load: true,
        disk: false,
        network: false,
        ..SystemConfig::default()
    };
    let collectors = super::init_collectors(&config);
    assert_eq!(collectors.len(), 1);
    assert_eq!(collectors[0].name(), "load");
}
