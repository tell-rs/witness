use crate::metrics::Collector;
use crate::sink::{DryRun, Sink};

fn test_sink() -> Sink {
    Sink::dry_run(DryRun::new(), Default::default())
}

#[test]
fn load_returns_values() {
    let sink = test_sink();
    let mut collector = super::load::LoadCollector;
    let mut buf = String::new();

    // Should not panic
    collector.collect(&sink, "test", &mut buf);
}

#[test]
fn test_load_per_cpu_is_load1_over_online_cpus() {
    use crate::sink::Capture;

    let cap = Capture::new();
    let sink = Sink::capture(cap.clone(), Default::default());
    let mut collector = super::load::LoadCollector;
    let mut buf = String::new();

    collector.collect(&sink, "test", &mut buf);

    let load1 = cap.metric_values("system.load.1");
    let per_cpu = cap.metric_values("system.load.1_per_cpu");
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    assert_eq!(per_cpu.len(), 1);
    assert!((per_cpu[0] - load1[0] / n as f64).abs() < 1e-9);
}
