#[macro_use]
extern crate rocket;

mod cli;
mod rocket_mounts;

use cli::*;
use datalith_core::{Datalith, DatalithManager};
use rocket::{Ignite, Rocket};

fn main() -> anyhow::Result<()> {
    let args = get_args();

    let rocket = rocket_mounts::create(args.address, args.listen_port, args.max_file_size.as_u64());

    rocket::execute(async {
        let datalith = Datalith::new(args.environment).await?;

        datalith.set_temporary_file_lifespan(args.temporary_file_lifespan);

        #[cfg(feature = "image-convert")]
        {
            datalith.set_max_image_resolution(args.max_image_resolution);
            datalith.set_max_image_resolution_multiplier(args.max_image_resolution_multiplier);
        }

        let datalith = DatalithManager::new(datalith).await?;

        let rocket = rocket.manage(datalith);

        Ok(rocket.launch().await?) as anyhow::Result<Rocket<Ignite>>
    })?;

    Ok(())
}
