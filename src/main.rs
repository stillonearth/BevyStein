mod actions;
mod actors;
mod ai;
mod animations;
mod app_states;
mod assets;
mod control;
mod events;
mod game;
mod gym;
mod hud;
mod level;
mod map;
mod physics;
mod render;
mod screens;

use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long)]
    mode: String,
}

fn main() {
    let args = Args::parse();

    let mut bevy_app = game::build_game_app(args.mode);
    bevy_app.run();
}
