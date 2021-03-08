use tokio::sync::watch;

pub async fn chrome_controller(mut chrome_remote_debugging_port_rx: watch::Receiver<Option<u16>>) {
    loop {
        chrome_remote_debugging_port_rx.changed().await.unwrap();
        println!(
            "Received chrome remote debugging port {:?}",
            *chrome_remote_debugging_port_rx.borrow()
        );
    }
}
