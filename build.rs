// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

#[macro_use]
extern crate amplify_derive;
#[macro_use]
extern crate clap;

use clap::IntoApp;
use clap_complete::generate_to;
use clap_complete::shells::*;

pub mod opts {
    include!("src/opts.rs");
}

pub mod cli {
    include!("src/cli/opts.rs");
}
pub mod farcasterd {
    include!("src/farcasterd/opts.rs");
}
pub mod peerd {
    include!("src/peerd/opts.rs");
}
pub mod swapd {
    include!("src/swapd/opts.rs");
}
pub mod syncerd {
    include!("src/syncerd/opts.rs");
}
pub mod walletd {
    include!("src/walletd/opts.rs");
}

fn main() -> Result<(), configure_me_codegen::Error> {
    let outdir = "./shell";

    for app in [
        farcasterd::Opts::command(),
        peerd::Opts::command(),
        swapd::Opts::command(),
        cli::Opts::command(),
        walletd::Opts::command(),
        syncerd::Opts::command(),
    ]
    .iter_mut()
    {
        let name = app.get_name().to_string();
        generate_to(Bash, app, &name, &outdir)?;
        generate_to(PowerShell, app, &name, &outdir)?;
        generate_to(Zsh, app, &name, &outdir)?;
    }

    configure_me_codegen::build_script_auto()
}
