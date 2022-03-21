use std::io::Cursor;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::thread;

use futures::executor;
use image;

use bevy::{
    core_pipeline::{
        draw_3d_graph, node, AlphaMask3d, Opaque3d, RenderTargetClearColors, Transparent3d,
    },
    prelude::*,
    render::{
        camera::{ActiveCamera, CameraTypePlugin, RenderTarget},
        render_graph::{Node, NodeRunError, RenderGraph, RenderGraphContext, SlotValue},
        render_phase::RenderPhase,
        render_resource::{
            Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
        },
        renderer::RenderContext,
        RenderApp, RenderStage,
    },
};

use bevy::render::render_asset::RenderAssets;
use bevy::render::renderer::RenderDevice;
use bevy::render::renderer::RenderQueue;

use bytemuck;

use wgpu::ImageCopyBuffer;
use wgpu::ImageDataLayout;

use gotham::helpers::http::response::create_response;
use gotham::middleware::state::StateMiddleware;
use gotham::pipeline::{single_middleware, single_pipeline};
use gotham::router::builder::*;
use gotham::router::Router;
use gotham::state::StateData;
use gotham::state::{FromState, State};
use hyper::{body, Body, Response, StatusCode};

#[derive(Clone)]
pub struct AIGymSettings {
    pub width: u32,
    pub height: u32,
}

pub trait Action {
    fn derive(self);
}

#[derive(Clone, Default)]
pub struct AIGymState<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe> {
    // These parts are made of hack trick internals.
    pub __render_target: Option<RenderTarget>, // render target for camera -- window on in our case texture
    pub __render_image_handle: Option<Handle<Image>>, // handle to image we use in bevy UI building.
    // actual texture is GPU ram and we can't access it easily
    pub __is_environment_paused: bool, // once set true we loop and wait until simulation epoch is finished
    pub __action_unparsed_string: String, // we receive action as post parameter and parse it in bevy system
    // Communication via mutex works but semantics are not straightforward.
    // We keep it hacky or else it could become java boilerplate.
    pub __request_for_reset: bool,

    // State
    pub screen: Option<image::RgbaImage>,
    pub rewards: Vec<f32>,
    pub action: Option<T>,
    pub is_terminated: bool,
}

#[derive(Component, Default)]
pub struct FirstPassCamera;

#[derive(Component)]
pub struct RenderComponent;

#[derive(Default, Clone)]
pub struct AIGymPlugin<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    pub PhantomData<T>,
);

impl<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe> Plugin for AIGymPlugin<T> {
    fn build(&self, app: &mut App) {
        const FIRST_PASS_DRIVER: &str = "first_pass_driver";

        app.add_plugin(CameraTypePlugin::<FirstPassCamera>::default());
        app.add_startup_system(setup::<T>.label("setup_rendering"));

        let ai_gym_state = app
            .world
            .get_resource::<Arc<Mutex<AIGymState<T>>>>()
            .unwrap()
            .clone();

        let ai_gym_settings = app.world.get_resource::<AIGymSettings>().unwrap().clone();

        // Render app
        let render_app = app.sub_app_mut(RenderApp);
        let driver = FirstPassCameraDriver::new(&mut render_app.world);
        // This will add 3D render phases for the new camera.
        render_app.add_system_to_stage(RenderStage::Extract, extract_first_pass_camera_phases);
        render_app.add_system_to_stage(RenderStage::Render, save_image::<T>);
        render_app.insert_resource(ai_gym_state.clone());
        render_app.insert_resource(ai_gym_settings.clone());

        let mut graph = render_app.world.resource_mut::<RenderGraph>();

        // Add a node for the first pass.
        graph.add_node(FIRST_PASS_DRIVER, driver);

        // The first pass's dependencies include those of the main pass.
        graph
            .add_node_edge(node::MAIN_PASS_DEPENDENCIES, FIRST_PASS_DRIVER)
            .unwrap();

        // Insert the first pass node: CLEAR_PASS_DRIVER -> FIRST_PASS_DRIVER -> MAIN_PASS_DRIVER
        graph
            .add_node_edge(node::CLEAR_PASS_DRIVER, FIRST_PASS_DRIVER)
            .unwrap();

        thread::spawn(move || {
            gotham::start(
                "127.0.0.1:7878",
                router::<T>(GothamState {
                    inner: ai_gym_state,
                }),
            )
        });
    }
}

// ------------------
// Rendering to Image
// ------------------

// Add 3D render phases for FIRST_PASS_CAMERA.
fn extract_first_pass_camera_phases(
    mut commands: Commands,
    active: Res<ActiveCamera<FirstPassCamera>>,
) {
    if let Some(entity) = active.get() {
        commands
            .get_or_spawn(entity)
            .insert_bundle((
                RenderPhase::<Opaque3d>::default(),
                RenderPhase::<AlphaMask3d>::default(),
                RenderPhase::<Transparent3d>::default(),
            ))
            .insert(RenderComponent);
    }
}

struct FirstPassCameraDriver {
    query: QueryState<Entity, With<FirstPassCamera>>,
}

impl FirstPassCameraDriver {
    pub fn new(render_world: &mut World) -> Self {
        Self {
            query: QueryState::new(render_world),
        }
    }
}

impl Node for FirstPassCameraDriver {
    fn update(&mut self, world: &mut World) {
        self.query.update_archetypes(world);
    }

    fn run(
        &self,
        graph: &mut RenderGraphContext,
        _render_context: &mut RenderContext,
        world: &World,
    ) -> Result<(), NodeRunError> {
        for camera in self.query.iter_manual(world) {
            graph.run_sub_graph(draw_3d_graph::NAME, vec![SlotValue::Entity(camera)])?;
        }
        Ok(())
    }
}

pub fn texture_image_layout(desc: &TextureDescriptor<'_>) -> ImageDataLayout {
    let size = desc.size;

    let layout = ImageDataLayout {
        bytes_per_row: if size.height > 1 {
            NonZeroU32::new(size.width * (desc.format.describe().block_size as u32))
        } else {
            None
        },
        rows_per_image: if size.depth_or_array_layers > 1 {
            NonZeroU32::new(size.height)
        } else {
            None
        },
        ..Default::default()
    };

    return layout;
}

fn save_image<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    gpu_images: Res<RenderAssets<Image>>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    ai_gym_state: Res<Arc<Mutex<AIGymState<T>>>>,
    ai_gym_settings: Res<AIGymSettings>,
) {
    let mut ai_gym_state = ai_gym_state.lock().unwrap();

    let gpu_image = gpu_images
        .get(&ai_gym_state.__render_image_handle.clone().unwrap())
        .unwrap();

    let device = render_device.wgpu_device();

    let destination = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (gpu_image.size.width * gpu_image.size.height * 4.0) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let texture = gpu_image.texture.clone();

    let mut encoder =
        render_device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

    let size = Extent3d {
        width: ai_gym_settings.width,
        height: ai_gym_settings.height,
        ..default()
    };

    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        ImageCopyBuffer {
            buffer: &destination,
            layout: texture_image_layout(&TextureDescriptor {
                label: Some("render_image"),
                size,
                dimension: TextureDimension::D2,
                format: TextureFormat::Bgra8UnormSrgb,
                mip_level_count: 1,
                sample_count: 1,
                usage: TextureUsages::TEXTURE_BINDING
                    | TextureUsages::COPY_DST
                    | TextureUsages::RENDER_ATTACHMENT, // | TextureUsages::STORAGE_BINDING,
            }),
        },
        Extent3d {
            width: gpu_image.size.width as u32,
            height: gpu_image.size.height as u32,
            ..default()
        },
    );

    render_queue.submit([encoder.finish()]);

    let buffer_slice = destination.slice(..);
    let buffer_future = buffer_slice.map_async(wgpu::MapMode::Read);
    device.poll(wgpu::Maintain::Wait);

    let result = executor::block_on(buffer_future);

    let err = result.err();
    if err.is_some() {
        panic!("{}", err.unwrap().to_string());
    }

    let data = buffer_slice.get_mapped_range();
    let result: Vec<u8> = bytemuck::cast_slice(&data).to_vec();

    // free memory
    drop(data);
    destination.unmap();

    let img: image::RgbaImage = image::ImageBuffer::from_raw(
        gpu_image.size.width as u32,
        gpu_image.size.height as u32,
        result,
    )
    .unwrap();
    ai_gym_state.screen = Some(img.clone());
}

fn setup<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    ai_gym_state: ResMut<Arc<Mutex<AIGymState<T>>>>,
    ai_gym_settings: Res<AIGymSettings>,
    mut clear_colors: ResMut<RenderTargetClearColors>,
    mut windows: ResMut<Windows>,
) {
    let size = Extent3d {
        width: ai_gym_settings.width,
        height: ai_gym_settings.height,
        ..default()
    };

    // This is the texture that will be rendered to.
    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: Some("render_image"),
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb, // ::Bgra8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
        },
        ..default()
    };

    // fill image.data with zeroes
    image.resize(size);

    let image_handle = images.add(image);

    let mut ai_gym_state = ai_gym_state.lock().unwrap();

    ai_gym_state.__render_target = Some(RenderTarget::Image(image_handle.clone()));
    ai_gym_state.__render_image_handle = Some(image_handle.clone());

    clear_colors.insert(ai_gym_state.__render_target.clone().unwrap(), Color::WHITE);

    // UI viewport for game
    commands
        .spawn_bundle(UiCameraBundle::default())
        .insert(RenderComponent);
    commands
        .spawn_bundle(ImageBundle {
            style: Style {
                align_self: AlignSelf::Center,
                ..Default::default()
            },
            image: image_handle.clone().into(),
            ..Default::default()
        })
        .insert(RenderComponent);

    let window = windows.get_primary_mut().unwrap();
    window.set_resolution(ai_gym_settings.width as f32, ai_gym_settings.height as f32);
    window.set_resizable(false);
}

// ---------------
// AI Gym REST API
// ---------------

#[derive(Clone, StateData)]
struct GothamState<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe> {
    inner: Arc<Mutex<AIGymState<T>>>,
}

fn router<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    state: GothamState<T>,
) -> Router {
    let middleware = StateMiddleware::new(state);
    let pipeline = single_middleware(middleware);

    let (chain, pipelines) = single_pipeline(pipeline);

    // build a router with the chain & pipeline
    build_router(chain, pipelines, |route| {
        route.get("/screen.png").to(screen::<T>);
        route.post("/step").to(step::<T>);
        route.post("/reset").to(reset::<T>);
    })
}

fn screen<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    state: State,
) -> (State, Response<Body>) {
    let state_: &GothamState<T> = GothamState::borrow_from(&state);
    let state__ = state_.inner.lock().unwrap().clone();
    let image = state__.screen.clone().unwrap();

    let mut bytes: Vec<u8> = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageOutputFormat::Png)
        .unwrap();

    let response = create_response::<Vec<u8>>(&state, StatusCode::OK, mime::TEXT_PLAIN, bytes);

    return (state, response);
}
fn step<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    mut state: State,
) -> (State, String) {
    let body_ = Body::take_from(&mut state);
    let valid_body = executor::block_on(body::to_bytes(body_)).unwrap();
    let action = String::from_utf8(valid_body.to_vec()).unwrap();

    let state_: &GothamState<T> = GothamState::borrow_from(&state);

    {
        let mut ai_gym_state = state_.inner.lock().unwrap();
        ai_gym_state.__action_unparsed_string = action;
    }

    let mut reward = 0.0;
    let is_terminated;
    loop {
        let ai_gym_state = state_.inner.lock().unwrap();

        if ai_gym_state.__is_environment_paused {
            if ai_gym_state.rewards.len() > 0 {
                reward = ai_gym_state.rewards[ai_gym_state.rewards.len() - 1];
            }
            if ai_gym_state.rewards.len() > 1 {
                reward -= ai_gym_state.rewards[ai_gym_state.rewards.len() - 2];
            }

            is_terminated = ai_gym_state.is_terminated.clone();

            break;
        }
    }

    return (
        state,
        format!(
            "{{\"reward\": {}, \"is_terminated\": {}}}",
            reward, is_terminated
        ),
    );
}

fn reset<T: 'static + Send + Sync + Clone + std::panic::RefUnwindSafe>(
    state: State,
) -> (State, String) {
    {
        let state_: &GothamState<T> = GothamState::borrow_from(&state);
        let mut ai_gym_state = state_.inner.lock().unwrap();
        ai_gym_state.__request_for_reset = true;
    }
    return (state, "ok".to_string());
}
