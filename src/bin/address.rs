//! Pops first address from the first line of file containing bitcoin addresses
//! separated by newline ("\n")

use std::io::{self, Write};
use std::str::FromStr;

pub fn pop_address_btc(path: &str) -> bitcoin::Address {
    let mut address =
        FromStr::from_str("tb1qa83aeqmfvn23llr2zc3gfkrwt8xvpv2k2cluzg").expect("Parsable address");
    // use address from first line and drop that line
    match set_addr(&mut address, path) {
        Ok(()) => address,
        Err(e) => {
            eprintln!(
                "make sure {} exists populated with bitcoin addresses you control \
                 such as: \n\ntb1qh0hgmalfuancfe28wnmrp0lctlsdxqf2fcqlsh\n\
                 tb1qgd0m0qwssw7q8t8whh2d0ala0g8xqfq976y9eu\n\n\n\
                 with no extra spaces, now defaulting to hardcoded address {} \
                 {}",
                path, address, e
            );
            address
        }
    }
}

pub fn pop_address_xmr(path: &str) -> monero::Address {
    let mut address = FromStr::from_str("7Bec8paDCCmYwL2fhyVsEXEAfhGeLb7BYjFk3amiPjheaBwhirD4af1WFBhWW4kiHEAjjMxPNJeucNBXvBKmeF2gEEqVYmk")
        .expect("Parsable address");
    // use address from first line and drop that line
    match set_addr(&mut address, path) {
        Ok(()) => address,
        Err(e) => {
            eprintln!(
                "make sure {} exists (and contains string 'btc' or 'xmr') populated with bitcoin addresses you control \
                 such as: \n\n7Bec8paDCCmYwL2fhyVsEXEAfhGeLb7BYjFk3amiPjheaBwhirD4af1WFBhWW4kiHEAjjMxPNJeucNBXvBKmeF2gEEqVYmk\n\
                 7AQ5MQ585JdW7LUNQ7oC7kLqRnJBx5CcQXVjyxk5Z8x62Xgd69QrDL5EjFcBukkFPL3HSBQNddzdX1WwS2HKCa572rpg7gH\n\n\n\
                 with no extra spaces, now defaulting to hardcoded address {} \
                 {}",
                path, address, e
            );
            address
        }
    }
}

fn set_addr(address: &mut impl FromStr, path: &str) -> io::Result<()> {
    let updated_lines = read_lines(path)?
        .enumerate()
        .filter_map(|(ix, line)| {
            let addr = line.ok()?;
            if ix == 0 {
                *address = FromStr::from_str(&addr).ok()?;
                // consume 1st line
                None
            } else {
                Some(addr)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::File::create(path)?.write_all(updated_lines.as_ref())?;
    Ok(())
}

// The output is wrapped in a Result to allow matching on errors
// Returns an Iterator to the Reader of the lines of the file.
fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<std::fs::File>>>
where
    P: AsRef<std::path::Path>,
{
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(filename)?;
    Ok(io::BufRead::lines(io::BufReader::new(file)))
}

pub fn main() {
    let args: Vec<_> = std::env::args().collect();
    let path = args[1].as_str();
    let address = {
        let xmr = path
            .to_lowercase()
            .find("xmr")
            .map(|_| pop_address_xmr(path).to_string());
        let btc = path
            .to_lowercase()
            .find("btc")
            .map(|_| pop_address_btc(path).to_string());
        match (btc, xmr) {
            (Some(btc), None) => btc,
            (None, Some(xmr)) => xmr,
            _ => "".to_string(),
        }
    };
    let _ = io::stdout().write(address.as_bytes()).unwrap();
}
