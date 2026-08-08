// Windows のリリースビルドでコンソールウィンドウを出さない
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    sview_lib::run()
}
