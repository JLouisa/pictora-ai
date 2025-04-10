use async_trait::async_trait;
use axum::{Extension, Router as AxumRouter};
use fluent_templates::{ArcLoader, FluentLoader};
use loco_rs::{
    app::{AppContext, Initializer},
    controller::views::{engines, ViewEngine},
    Error, Result,
};
use tracing::info;

use crate::{
    domain::settings::Settings,
    service::{aws::s3::AwsS3, stripe::stripe::StripeClient},
};

#[allow(clippy::module_name_repetitions)]
pub struct Stripe;

#[async_trait]
impl Initializer for Stripe {
    fn name(&self) -> String {
        "stripe".to_string()
    }

    async fn after_routes(&self, router: AxumRouter, ctx: &AppContext) -> Result<AxumRouter> {
        let stripe_settings: Settings =
            serde_json::from_value(ctx.config.settings.clone().expect("No settings found"))
                .expect("Failed to parse settings");

        let stripe_client = StripeClient::new(&stripe_settings.stripe);

        let router = router.layer(Extension(stripe_client));
        Ok(router)
    }
}
