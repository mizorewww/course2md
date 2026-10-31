//! The names old pages call for icons, each now a Material Symbol. New code calls
//! `symbols::*` directly (docs/split/MIGRATION.md). Goes with the last old page.
use crate::symbols;
use gpui::Styled;
use gpui_kit::component::Icon;

macro_rules! icons {
    ($($name:ident => $symbol:ident;)+) => {
        $(
            pub fn $name() -> Icon {
                symbols::$symbol().component()
            }
        )+
    };
}

icons! {
    arrow_forward => arrow_forward;
    arrow_left => arrow_back;
    article => article;
    auto_fix => auto_fix_high;
    book_open => menu_book;
    braces => data_object;
    check => check;
    check_circle => check_circle;
    chevron_down => keyboard_arrow_down;
    chevron_left => chevron_left;
    chevron_right => chevron_right;
    chevron_up => keyboard_arrow_up;
    close => close;
    cloud => cloud;
    code => code;
    computer => computer;
    content_copy => content_copy;
    delete => delete;
    cpu => memory;
    download => download;
    edit => edit;
    error => cancel;
    external_link => open_in_new;
    eye => visibility;
    eye_off => visibility_off;
    file_text => description;
    file_upload => file_upload;
    folder => folder;
    folder_input => drive_file_move;
    folder_open => folder_open;
    folder_plus => create_new_folder;
    globe => language;
    gpu => developer_board;
    grip_vertical => drag_indicator;
    image => image;
    inbox => inbox;
    info => info;
    layout_grid => grid_view;
    library => library_books;
    link => link;
    list => list;
    list_checks => checklist;
    loader_circle => progress_activity;
    login => login;
    logout => logout;
    maximize => open_in_full;
    message_square => chat_bubble;
    mic => mic;
    microchip => memory;
    moon => dark_mode;
    more_vertical => more_vert;
    notebook => book;
    palette => palette;
    play => play_arrow;
    plus => add;
    refresh => refresh;
    restart => restart_alt;
    save => save;
    schedule => schedule;
    science => science;
    search => search;
    server => dns;
    settings => settings;
    sparkles => auto_awesome;
    storage => storage;
    subtitles => subtitles;
    sun => light_mode;
    text_lines => notes;
    tune => tune;
    tv => tv;
    warning => warning;
    arrow_down => arrow_downward;
    arrow_up => arrow_upward;
    fit_screen => fit_screen;
    history => history;
    movie => movie;
    pause => pause;
    play_arrow => play_arrow;
    summarize => summarize;
    task => task;
    toc => format_list_bulleted;
    zoom_in => zoom_in;
    zoom_out => zoom_out;
}

pub fn bilibili_mark() -> Icon {
    Icon::default().path("icons/bilibili.svg")
}

/// Brand colors are intrinsic to the platform mark, independent of UI themes.
pub fn bilibili() -> Icon {
    bilibili_mark().text_color(gpui::rgb(0x00a1d6))
}

pub fn caption_close() -> Icon {
    symbols::close().component()
}

pub fn caption_maximize() -> Icon {
    symbols::crop_square().component()
}

pub fn caption_minimize() -> Icon {
    symbols::remove().component()
}

pub fn caption_restore() -> Icon {
    symbols::filter_none().component()
}
