//! The application's icon set. Each name has exactly one asset: Lucide outline
//! icons for shared chrome, Material icons where Lucide has no match, and the
//! platform brand marks. GPUI Component looks icons up by `icons/<file>.svg`,
//! so a registered file also replaces the component's own icon of that name.
use gpui::{AssetSource, SharedString, Styled};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:ident => $dir:literal / $file:literal;)+) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((
                concat!("icons/", $file, ".svg"),
                include_bytes!(concat!("../assets/", $dir, "/", $file, ".svg")),
            ),)+
        ];
        $(
            pub fn $name() -> gpui_component::Icon {
                gpui_component::Icon::default().path(concat!("icons/", $file, ".svg"))
            }
        )+
    };
}

icons! {
    arrow_forward => "outline" / "arrow-forward";
    arrow_left => "outline" / "arrow-left";
    article => "outline" / "article";
    auto_fix => "outline" / "auto-fix";
    book_open => "outline" / "book-open";
    braces => "outline" / "braces";
    check => "outline" / "check";
    check_circle => "outline" / "circle-check";
    chevron_down => "outline" / "chevron-down";
    chevron_left => "outline" / "chevron-left";
    chevron_right => "outline" / "chevron-right";
    chevron_up => "outline" / "chevron-up";
    close => "outline" / "close";
    cloud => "outline" / "cloud";
    code => "outline" / "code";
    computer => "outline" / "computer";
    content_copy => "outline" / "content-copy";
    delete => "outline" / "trash";
    cpu => "outline" / "cpu";
    download => "outline" / "download";
    edit => "outline" / "edit";
    error => "outline" / "circle-x";
    external_link => "outline" / "external-link";
    eye => "outline" / "eye";
    eye_off => "outline" / "eye-off";
    file_text => "outline" / "file-text";
    file_upload => "outline" / "file-upload";
    folder => "outline" / "folder";
    folder_input => "outline" / "folder-input";
    folder_open => "outline" / "folder-open";
    folder_plus => "outline" / "folder-plus";
    globe => "outline" / "globe";
    gpu => "outline" / "gpu";
    grip_vertical => "outline" / "grip-vertical";
    inbox => "outline" / "inbox";
    info => "outline" / "info";
    layout_grid => "outline" / "layout-grid";
    library => "outline" / "library";
    link => "outline" / "link";
    list => "outline" / "list";
    list_checks => "outline" / "list-checks";
    loader_circle => "outline" / "loader-circle";
    login => "outline" / "login";
    logout => "outline" / "logout";
    maximize => "outline" / "maximize-2";
    message_square => "outline" / "message-square";
    mic => "outline" / "mic";
    microchip => "outline" / "microchip";
    moon => "outline" / "moon";
    more_vertical => "outline" / "ellipsis-vertical";
    notebook => "outline" / "notebook";
    palette => "outline" / "palette";
    play => "outline" / "play";
    plus => "outline" / "plus";
    refresh => "outline" / "refresh";
    restart => "outline" / "restart";
    save => "outline" / "save";
    schedule => "outline" / "schedule";
    science => "outline" / "science";
    search => "outline" / "search";
    server => "outline" / "server";
    settings => "outline" / "settings";
    sparkles => "outline" / "sparkles";
    storage => "outline" / "storage";
    subtitles => "outline" / "captions";
    sun => "outline" / "sun";
    text_lines => "outline" / "text-align-start";
    tune => "outline" / "tune";
    tv => "outline" / "tv";
    warning => "outline" / "triangle-alert";

    arrow_down => "material" / "arrow-down";
    arrow_up => "material" / "arrow-up";
    ellipsis => "material" / "ellipsis";
    fit_screen => "material" / "fit-screen";
    history => "material" / "history";
    image => "material" / "image";
    movie => "material" / "movie";
    pause => "material" / "pause";
    play_arrow => "material" / "play-arrow";
    summarize => "material" / "summarize";
    task => "material" / "task";
    toc => "material" / "format-list-bulleted";
    zoom_in => "material" / "zoom-in";
    zoom_out => "material" / "zoom-out";

    bilibili_mark => "brands" / "bilibili";
}

const APP_ICON: (&str, &[u8]) = (
    "images/course2md.png",
    include_bytes!("../assets/128x128@2x.png"),
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(formula) = crate::math::asset(path) {
            return Ok(Some(formula));
        }
        if let Some((_, bytes)) = ICONS
            .iter()
            .chain([&APP_ICON])
            .find(|(name, _)| *name == path)
        {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit_assets::Assets.load(path)
    }
    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(
            ICONS
                .iter()
                .chain([&APP_ICON])
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// A new folder: the outline `folder_plus`, under the name some callers use.
pub fn create_new_folder() -> gpui_component::Icon {
    folder_plus()
}

/// Brand colors are intrinsic to the platform mark, independent of UI themes.
pub fn bilibili() -> gpui_component::Icon {
    bilibili_mark().text_color(gpui::rgb(0x00a1d6))
}
