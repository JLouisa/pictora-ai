#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unnecessary_struct_initialization)]
#![allow(clippy::unused_async)]
use loco_rs::prelude::*;

use crate::domain::domain_services::image_generation::ImageGenerationService;
use crate::domain::url::Url;
use crate::domain::website::Website;
use crate::models::_entities::sea_orm_active_enums::{ImageFormat, ImageSize, Status};
use crate::models::images::{AltText, ImageNew, ImageNewList, UserPrompt};
use crate::models::join::user_credits_models::load_user_and_credits;
use crate::models::join::user_image::load_user_and_image;
use crate::models::users::UserPid;
use crate::models::{ImageActiveModel, ImageModel, TrainingModelModel, UserCreditModel, UserModel};
use crate::service::aws::s3::{AwsS3, S3Folders};
use crate::service::redis::redis::Cache;
use crate::views::images::{CreditsViewModel, ImageViewList, ImageViewModel};
use crate::{models::_entities::images::Entity, service::fal_ai::fal_client::FalAiClient, views};
use axum::{debug_handler, Extension};
use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone, Validate, Debug, Deserialize)]
pub struct ImageGenRequestParams {
    pub training_model_id: i32,
    pub prompt: UserPrompt,
    pub image_size: ImageSize,
    #[validate(range(min = 1, max = 50, message = "Creative must be between 1 and 50"))]
    pub num_inference_steps: u8,
    #[validate(range(
        min = 1,
        max = 20,
        message = "Number of images must be between 1 and 20"
    ))]
    pub num_images: u8,
}
impl ImageGenRequestParams {
    pub fn process(self, model: &TrainingModelModel) -> ImageNewList {
        let sys_prompt = self.prompt.formatted_prompt(model);
        let alt = AltText::from(&self.prompt);
        (0..self.num_images)
            .map(|_| ImageNew {
                pid: Uuid::new_v4(),
                user_id: model.user_id,
                training_model_id: self.training_model_id,
                pack_id: None,
                sys_prompt: sys_prompt.to_owned(),
                user_prompt: self.prompt.to_owned(),
                alt: alt.to_owned(),
                num_inference_steps: self.num_inference_steps as i32,
                content_type: ImageFormat::Jpeg,
                status: Status::Pending,
                image_size: self.image_size,
                fal_ai_request_id: None,
                width: None,
                height: None,
                image_url_fal: None,
                image_s3_key: None,
                is_favorite: false,
                deleted_at: None,
            })
            .collect::<Vec<ImageNew>>()
            .into()
    }
}

pub mod routes {
    use serde::Serialize;

    #[derive(Clone, Debug, Serialize)]
    pub struct Images;
    impl Images {
        pub const BASE: &'static str = "/api/images";
        pub const IMAGE: &'static str = "/";
        pub const IMAGE_S3_UPLOAD_COMPLETE: &'static str = "/upload/complete";
        pub const IMAGE_S3_UPLOAD_COMPLETE_ID: &'static str = "/upload/complete/{id}";
        pub const IMAGE_GENERATE: &'static str = "/generate";
        pub const IMAGE_GENERATE_TEST: &'static str = "/generate/test";
        pub const IMAGE_CHECK_TEST: &'static str = "/check/test/{id}";
        pub const IMAGE_CHECK_ID: &'static str = "/check/{id}";
        pub const IMAGE_CHECK: &'static str = "/check";
        pub const IMAGE_ID: &'static str = "/{id}";
        pub const IMAGE_RESTORE_ID: &'static str = "/restore/{id}";
        pub const IMAGE_RESTORE: &'static str = "/restore";
        pub const IMAGE_BASE: &'static str = "";

        pub fn check_route() -> String {
            use crate::controllers::images;

            let check_route = format!(
                "{}{}/test",
                images::routes::Images::BASE,
                images::routes::Images::IMAGE_CHECK
            );
            check_route
        }
    }
}

pub fn routes() -> Routes {
    Routes::new()
        .prefix(routes::Images::BASE)
        .add(routes::Images::IMAGE, get(list))
        // .add(routes::Images::IMAGE, post(add))
        .add(routes::Images::IMAGE_GENERATE_TEST, post(generate_test))
        // .add(routes::Images::IMAGE_GENERATE, post(generate_img))
        .add(routes::Images::IMAGE_CHECK_TEST, get(check_test))
        .add(routes::Images::IMAGE_CHECK_ID, get(check_img))
        .add(routes::Images::IMAGE_ID, get(get_one))
        .add(routes::Images::IMAGE_ID, delete(remove))
        .add(routes::Images::IMAGE_RESTORE_ID, delete(restore))
        .add(
            routes::Images::IMAGE_S3_UPLOAD_COMPLETE_ID,
            patch(upload_img_s3_completed),
        )
    // .add(routes::Images::IMAGE_ID, put(update))
    // .add(routes::Images::IMAGE_ID, patch(update))
}

async fn load_user(db: &DatabaseConnection, pid: &str) -> Result<UserModel> {
    let item = UserModel::find_by_pid(db, pid).await?;
    Ok(item)
}
async fn load_item(ctx: &AppContext, id: i32) -> Result<ImageModel> {
    let item = Entity::find_by_id(id).one(&ctx.db).await?;
    item.ok_or_else(|| Error::NotFound)
}
async fn load_item_pid(ctx: &AppContext, id: Uuid) -> Result<ImageModel> {
    let item = ImageModel::find_by_pid(&ctx.db, &id).await?;
    Ok(item)
}
async fn load_credits(db: &DatabaseConnection, id: i32) -> Result<UserCreditModel> {
    let credits = UserCreditModel::find_by_user_id(db, id).await?;
    Ok(credits)
}

#[debug_handler]
pub async fn upload_img_s3_completed(
    auth: auth::JWT,
    Path(img_pid): Path<Uuid>,
    Extension(s3_client): Extension<AwsS3>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let (user, image) = load_user_and_image(&ctx.db, &auth.claims.pid, &img_pid).await?;
    let s3_key = s3_client.create_s3_key(
        &user.pid,
        &S3Folders::Images,
        &image.pid.to_string(),
        &ImageFormat::Jpeg,
    );

    let exists = s3_client
        .check_object_exists(&s3_key)
        .await
        .map_err(|_| loco_rs::Error::Message(String::from("Error checking storage: 101")))?;

    if !exists {
        return Ok((StatusCode::NO_CONTENT).into_response().into_response());
    }

    ImageActiveModel::from(image)
        .upload_s3_completed(&s3_key, &ctx.db)
        .await
        .ok();

    Ok((StatusCode::OK).into_response())
}

#[debug_handler]
pub async fn check_test(
    auth: auth::JWT,
    Path(pid): Path<Uuid>,
    Extension(cache): Extension<Cache>,
    State(ctx): State<AppContext>,
    Extension(s3_client): Extension<AwsS3>,
    ViewEngine(v): ViewEngine<TeraView>,
) -> Result<Response> {
    use rand::Rng;
    let (user, mut image) = load_user_and_image(&ctx.db, &auth.claims.pid, &pid).await?;

    if image.user_id != user.id {
        return Err(Error::Unauthorized("Unauthorized".to_string()));
    }
    let change = rand::rng().random_range(0..=3);
    if change == 0 {
        let image_url_fal = Url::new("https://v3.fal.media/files/panda/ycu2NDkTawQBdmgZDAF3g_ffb513c9074146009320fa60e64beaab.jpg".to_string());
        image.image_url_fal = Some(image_url_fal.as_ref().to_owned());
        image.status = Status::Completed;
        image
            .clone()
            .update_fal_image_url(&image_url_fal, &ctx.db)
            .await?;
    }
    if image.status == Status::Completed {
        let user_credits = load_credits(&ctx.db, user.id).await?;
        let user_credits_view: CreditsViewModel = user_credits.into();
        let check_route = routes::Images::check_route();
        let image: ImageViewModel = image.into();
        let image: ImageViewModel = image
            .clone()
            .set_pre_url(&user.pid, &s3_client)
            .await
            .unwrap_or_else(|_| image);
        return views::images::img_completed(
            &v,
            &ImageViewList::new(vec![image]),
            check_route.as_str(),
            &user_credits_view,
        );
    }

    Ok((StatusCode::NO_CONTENT).into_response())
}

#[debug_handler]
pub async fn check_img(
    auth: auth::JWT,
    Path(pid): Path<Uuid>,
    Extension(cache): Extension<Cache>,
    State(ctx): State<AppContext>,
    Extension(s3_client): Extension<AwsS3>,
    ViewEngine(v): ViewEngine<TeraView>,
) -> Result<Response> {
    let user_pid = UserPid::new(&auth.claims.pid);
    let image = load_item_pid(&ctx, pid).await?;
    let (user, user_credits) = load_user_and_credits(&ctx.db, &user_pid).await?;

    if image.user_id != user.id {
        return Err(Error::Unauthorized("Unauthorized".to_string()));
    }

    if image.status == Status::Completed {
        let user_credits_view: CreditsViewModel = user_credits.into();
        let check_route = routes::Images::check_route();
        let image: ImageViewModel = image.into();
        let image: ImageViewModel = image
            .clone()
            .set_pre_url(&user.pid, &s3_client)
            .await
            .unwrap_or_else(|_| image);
        return views::images::img_completed(
            &v,
            &ImageViewList::new(vec![image]),
            check_route.as_str(),
            &user_credits_view,
        );
    }
    Ok((StatusCode::NO_CONTENT).into_response())
}

#[debug_handler]
pub async fn generate_test(
    auth: auth::JWT,
    State(ctx): State<AppContext>,
    Extension(fal_ai_client): Extension<FalAiClient>,
    ViewEngine(v): ViewEngine<TeraView>,
    Json(request): Json<ImageGenRequestParams>,
) -> Result<Response> {
    // 1. Validate request payload format
    request.validate()?;

    dbg!(&request);

    // 2. Call the Domain Service to perform the core logic
    let (updated_credits, saved_images) =
        ImageGenerationService::generate(&ctx, &fal_ai_client, &auth.claims.pid, request).await?;

    // 3. Prepare Data for the View using safe View Models
    let credits_view_model = CreditsViewModel::from(&updated_credits);
    let image_view_models: Vec<ImageViewModel> = saved_images.into();

    // 4. Prepare other view-specific data
    let check_route = format!(
        "{}{}/test",
        routes::Images::BASE,
        routes::Images::IMAGE_CHECK
    );

    // 5. Render the view using the View Models
    views::images::img_completed(
        &v,
        &image_view_models.into(),
        &check_route,
        &credits_view_model,
    )
}

#[debug_handler]
pub async fn get_one(
    auth: auth::JWT,
    Path(id): Path<Uuid>,
    State(ctx): State<AppContext>,
    ViewEngine(v): ViewEngine<TeraView>,
) -> Result<Response> {
    let user = UserModel::find_by_pid(&ctx.db, &auth.claims.pid).await?;
    let image = load_item_pid(&ctx, id).await?;
    if image.user_id != user.id {
        return Ok((StatusCode::UNAUTHORIZED).into_response());
    }
    views::images::show(&v, &image)
}
#[debug_handler]
pub async fn list(State(ctx): State<AppContext>) -> Result<Response> {
    format::json(Entity::find().all(&ctx.db).await?)
}

#[debug_handler]
pub async fn remove(
    auth: auth::JWT,
    Path(img_pid): Path<Uuid>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let (user, img) = load_user_and_image(&ctx.db, &auth.claims.pid, &img_pid).await?;
    if img.user_id != user.id {
        return Ok((StatusCode::UNAUTHORIZED).into_response());
    }
    img.delete_image(&ctx.db).await?;
    Ok((StatusCode::OK).into_response())
}

#[debug_handler]
pub async fn restore(
    auth: auth::JWT,
    Path(img_pid): Path<Uuid>,
    State(ctx): State<AppContext>,
) -> Result<Response> {
    let (user, img) = load_user_and_image(&ctx.db, &auth.claims.pid, &img_pid).await?;
    if img.user_id != user.id {
        return Ok((StatusCode::UNAUTHORIZED).into_response());
    }
    img.restore_image(&ctx.db).await?;
    Ok((StatusCode::OK).into_response())
}
