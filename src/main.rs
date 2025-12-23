mod cmd;
mod server;

use cmd::cli::Commands;
use cmd::prelude::*;
// use std::time::Duration;

// https://docs.rs/serialport/latest/serialport/
// https://github.com/serialport/serialport-rs

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = cmd::cli::Cli::parse();
    println!("debug level = {}", cli.debug);

    match &cli.command {
        Commands::Serve(args) => cmd::serve::run(args).await?,
        Commands::Console(args) => cmd::console::run(args)?,
    }

    // let ports = serialport::available_ports().expect("No ports found!");
    // for p in ports {
    //     println!("{}", p.port_name);
    // }

    // let mut port = serialport::new("/dev/ttyACM0", 115_200)
    //     .timeout(Duration::from_millis(100))
    //     .open()
    //     .expect("Failed to open port");
    // port.write("GLG\r\n".as_bytes()).expect("Write failed!");

    // // https://github.com/serialport/serialport-rs/blob/main/examples/receive_data.rs
    // let mut serial_buf: Vec<u8> = vec![0; 128];
    // loop {
    //     match port.read(serial_buf.as_mut_slice()) {
    //         Ok(t) => {
    //             println!("Read {} bytes", t);
    //             println!("{}", String::from_utf8_lossy(&serial_buf));
    //             break;
    //         }
    //         Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => (),
    //         Err(e) => eprintln!("{:?}", e),
    //     }
    // }
    Ok(())
}
