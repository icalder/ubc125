use std::time::Duration;

use clap::Args;

#[derive(Args)]
pub struct ConsoleArgs {
    #[arg(short, long, default_value_t = String::from("/dev/ttyACM0"))]
    pub console_device: String,
}

pub fn run(args: &ConsoleArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Connecting to console device at {}", args.console_device);

    let ports = serialport::available_ports().expect("No ports found!");
    if ports.len() == 0 {
        println!("No serial ports found!");
        return Ok(());
    }
    for p in &ports {
        println!("{}", p.port_name);
    }
    let mut port = serialport::new("/dev/ttyACM0", 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .expect("Failed to open port");
    port.write("GLG\r\n".as_bytes()).expect("Write failed!");

    // https://github.com/serialport/serialport-rs/blob/main/examples/receive_data.rs
    let mut serial_buf: Vec<u8> = vec![0; 128];
    loop {
        match port.read(serial_buf.as_mut_slice()) {
            Ok(t) => {
                println!("Read {} bytes", t);
                println!("{}", String::from_utf8_lossy(&serial_buf));
                break;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => (),
            Err(e) => eprintln!("{:?}", e),
        }
    }
    Ok(())
}
