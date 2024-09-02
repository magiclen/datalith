mod cli;
mod rocket_mounts;

use cli::*;
use datalith_core::Datalith;
use rocket::{Ignite, Rocket};

fn main() -> anyhow::Result<()> {
    let args = get_args();

    let rocket = rocket_mounts::create(args.address, args.listen_port);

    rocket::execute(async {
        let datalith = Datalith::new(args.environment).await?;

        datalith.set_temporary_file_lifespan(args.temporary_file_lifespan);

        #[cfg(feature = "image-convert")]
        {
            datalith.set_max_image_resolution(args.max_image_resolution);
            datalith.set_max_image_resolution_multiplier(args.max_image_resolution_multiplier);
        }

        Ok(rocket.launch().await?) as anyhow::Result<Rocket<Ignite>>
    })?;

    Ok(())
}
