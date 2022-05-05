use rand::prelude::SliceRandom;
use rand::thread_rng;
use rand::Rng;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_mod_raycast::{RayCastMesh, RayCastSource};
use bevy_rl::{state::AIGymState, AIGymCamera};
use heron::*;

use names::Generator;

use crate::{actions::*, animations::*, assets::*, game::*, level::*, physics::*};

#[derive(Component, Clone)]
pub(crate) struct Actor {
    pub position: (f32, f32),
    pub rotation: f32,
    pub name: String,
    pub health: u16,
    pub score: u16,
}

#[derive(Component, Clone)]
pub(crate) struct PlayerPerspective;

#[derive(Bundle)]
pub(crate) struct ActorBundle {
    collision_layers: CollisionLayers,
    collision_shape: CollisionShape,
    global_transform: GlobalTransform,
    actor: Actor,
    rigid_body: RigidBody,
    rotation_constraints: RotationConstraints,
    transform: Transform,
    velocity: Velocity,
    physics_material: PhysicMaterial,
}

#[derive(Bundle)]
pub(crate) struct BillboardBundle {
    #[bundle]
    pbr_bundle: PbrBundle,
    billboard: Billboard,
    animation: EnemyAnimation,
    raycast_marker: RayCastMesh<RaycastMarker>,
    animation_timer: AnimationTimer,
}

fn new_actor_bundle(game_map: GameMap, actor_name: String) -> ActorBundle {
    let mut rng = thread_rng();
    let pos = game_map.empty_space.choose(&mut rng).unwrap();

    let actor = Actor {
        position: (pos.0 as f32, pos.1 as f32),
        rotation: rng.gen_range(0.0..std::f32::consts::PI * 2.0),
        name: actor_name,
        health: 100,
        score: 0,
    };

    return ActorBundle {
        transform: Transform {
            translation: Vec3::new(actor.position.0 as f32, 1.0, actor.position.1 as f32),
            rotation: Quat::from_rotation_y(actor.rotation),
            ..Default::default()
        },
        global_transform: GlobalTransform::identity(),
        velocity: Velocity::from_linear(Vec3::ZERO),
        collision_shape: CollisionShape::Sphere { radius: 1.0 },
        rigid_body: RigidBody::Dynamic,
        physics_material: PhysicMaterial {
            density: 200.0,
            ..Default::default()
        },
        collision_layers: CollisionLayers::new(Layer::Player, Layer::World),
        actor: actor,
        rotation_constraints: RotationConstraints::lock(),
    };
}

fn new_billboard_bundle(assets: GameAssets, mesh: Handle<Mesh>) -> BillboardBundle {
    return BillboardBundle {
        pbr_bundle: PbrBundle {
            mesh: mesh.clone(),
            material: assets.guard_billboard_material.clone(),
            transform: Transform {
                translation: Vec3::ZERO,
                ..Default::default()
            },
            ..Default::default()
        },
        billboard: Billboard,
        animation: EnemyAnimation {
            frame: 0,
            handle: mesh.clone(),
            animation_type: AnimationType::Standing,
        },
        animation_timer: AnimationTimer(Timer::from_seconds(0.3, true)),
        raycast_marker: RayCastMesh::<RaycastMarker>::default(),
    };
}

#[derive(Bundle)]
struct ActorWeaponBundle {
    #[bundle]
    pbr_bundle: PbrBundle,
    raycast_source: RayCastSource<RaycastMarker>,
}

fn new_actor_weapon_bundle(mesh: Handle<Mesh>) -> ActorWeaponBundle {
    return ActorWeaponBundle {
        pbr_bundle: PbrBundle {
            mesh: mesh,
            transform: Transform {
                translation: Vec3::ZERO,
                ..Default::default()
            },
            visibility: Visibility { is_visible: false },
            ..Default::default()
        },
        raycast_source: RayCastSource::<RaycastMarker>::new_transform_empty(),
    };
}

pub(crate) fn spawn_player_actor(
    mut commands: Commands,
    game_map: Res<GameMap>,
    mut meshes: ResMut<Assets<Mesh>>,
    ai_gym_state: Res<Arc<Mutex<AIGymState<PlayerActionFlags>>>>,
) {
    let ai_gym_state = ai_gym_state.lock().unwrap();
    let actor_bundle = new_actor_bundle(game_map.clone(), "Player 1".to_string());
    commands
        .spawn_bundle(actor_bundle)
        .insert(PlayerPerspective)
        .with_children(|cell| {
            cell.spawn_bundle(PointLightBundle {
                point_light: PointLight {
                    intensity: 500.0,
                    shadows_enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            });

            // Camera
            cell.spawn_bundle(PerspectiveCameraBundle::<AIGymCamera> {
                camera: Camera {
                    target: ai_gym_state.__render_target.clone().unwrap(),
                    ..default()
                },
                ..PerspectiveCameraBundle::new()
            })
            .insert(RayCastSource::<RaycastMarker>::new_transform_empty());

            // Hitbox
            let mesh = meshes.add(Mesh::from(shape::Quad::new(Vec2::new(0.8, 1.7))));
            cell.spawn_bundle(PbrBundle {
                mesh: mesh.clone(),
                transform: Transform {
                    rotation: Quat::from_rotation_y(std::f32::consts::PI),
                    ..Default::default()
                },
                visibility: Visibility { is_visible: true },
                ..Default::default()
            })
            .insert(RayCastMesh::<RaycastMarker>::default());
        });
}

pub(crate) fn spawn_computer_actors(
    mut commands: Commands,
    game_map: Res<GameMap>,
    mut meshes: ResMut<Assets<Mesh>>,
    game_sprites: Res<GameAssets>,
) {
    let enemy_count = 64;

    for _ in 0..enemy_count {
        let actor_bundle = new_actor_bundle(game_map.clone(), Generator::default().next().unwrap());

        commands.spawn_bundle(actor_bundle).with_children(|cell| {
            // Spawn soldier sprite
            let mut mesh = Mesh::from(shape::Quad::new(Vec2::new(0.8, 1.7)));
            let uv = game_sprites.guard_standing_animation[0][0].clone();
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
            let mesh = meshes.add(mesh);

            let billboard_bundle = new_billboard_bundle(game_sprites.clone(), mesh);
            cell.spawn_bundle(billboard_bundle);

            // Weapon
            let mesh = meshes.add(Mesh::from(shape::Quad::new(Vec2::new(0.8, 1.7))));
            let actor_weapon_bundle = new_actor_weapon_bundle(mesh);
            cell.spawn_bundle(actor_weapon_bundle);
        });
    }
}