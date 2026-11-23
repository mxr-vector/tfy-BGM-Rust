//! GPUI 专属外观与平台偏好。

use serde::{Deserialize, Serialize};

use bgm_core::{CloseAction, Prefs};
pub use bgm_core::{OutputRoute, prefs::MAX_OUTPUT_ROUTES};
use serde_json::{Map, Value};

pub fn decode_prefs(mut fields: Map<String, Value>) -> Result<Prefs, serde_json::Error> {
    if let Some(routes) = fields.get_mut("outputRoutes").and_then(Value::as_array_mut) {
        for route in routes.iter_mut().filter_map(Value::as_object_mut) {
            if route
                .get("deviceId")
                .and_then(Value::as_str)
                .is_some_and(|id| id.trim() != "default")
            {
                route.entry("label").or_insert_with(|| "输出设备".into());
            }
        }
    }
    serde_json::from_value(Value::Object(fields))
}

fn order_routes(routes: &mut Vec<OutputRoute>) {
    if let Some(index) = routes.iter().position(|route| route.device_id == "default") {
        let default = routes.remove(index);
        routes.insert(0, default);
    }
    routes.truncate(MAX_OUTPUT_ROUTES);
}

pub fn set_minimize_to_tray(prefs: &mut Prefs, enabled: bool) {
    prefs.minimize_to_tray = enabled;
    if !enabled && prefs.close_action == CloseAction::Tray {
        prefs.close_action = CloseAction::Ask;
    }
}

pub fn normalize_prefs(mut prefs: Prefs, rates: &[f32]) -> Prefs {
    prefs = prefs.normalize_with_rates(rates);
    prefs.ui_font = match prefs.ui_font.as_str() {
        ".SystemUIFont" => "system".to_owned(),
        "harmony" => "HarmonyOS Sans SC".to_owned(),
        "misans" => "MiSans".to_owned(),
        "pingfang" => "PingFang SC".to_owned(),
        "yahei" => "Microsoft YaHei".to_owned(),
        _ => prefs.ui_font,
    };
    let minimize_to_tray = prefs.minimize_to_tray;
    set_minimize_to_tray(&mut prefs, minimize_to_tray);
    order_routes(&mut prefs.output_routes);
    prefs
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    #[serde(other)]
    System,
}

impl Appearance {
    pub fn dark(self, system: gpui::WindowAppearance) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => matches!(
                system,
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bgm_core::prefs::PLAYBACK_RATES;
    use gpui::WindowAppearance;
    use serde_json::json;

    fn normalize_routes(output_routes: Vec<OutputRoute>) -> Vec<OutputRoute> {
        normalize_prefs(
            Prefs {
                output_routes,
                ..Prefs::default()
            },
            &PLAYBACK_RATES,
        )
        .output_routes
    }

    #[test]
    fn legacy_preferences_follow_system_and_migrate_system_font_alias() {
        let fields =
            json!({"uiFont":".SystemUIFont","autoplayOnStart":true,"crossfadeSeconds":4.0});
        let preferences = normalize_prefs(serde_json::from_value(fields).unwrap(), &PLAYBACK_RATES);
        assert_eq!(preferences.ui_font, "system");
        assert!(preferences.autoplay_on_start);
        assert_eq!(preferences.crossfade_seconds, 4.0);
        assert!(Appearance::System.dark(WindowAppearance::VibrantDark));
        assert!(!Appearance::System.dark(WindowAppearance::VibrantLight));
    }

    #[test]
    fn playback_preferences_do_not_serialize_appearance() {
        let fields = json!({"appearance":"dark","uiFont":"Helvetica Neue","crossfadeSeconds":3.5});
        let preferences = normalize_prefs(serde_json::from_value(fields).unwrap(), &PLAYBACK_RATES);
        assert!(Appearance::Dark.dark(WindowAppearance::Light));
        assert!(!Appearance::Light.dark(WindowAppearance::Dark));
        assert_eq!(preferences.ui_font, "Helvetica Neue");
        assert_eq!(preferences.crossfade_seconds, 3.5);
        let serialized = serde_json::to_value(preferences).unwrap();
        assert!(serialized.get("appearance").is_none());
    }

    #[test]
    fn output_limit_reserves_space_for_the_system_route() {
        let routes = (0..MAX_OUTPUT_ROUTES)
            .map(|index| OutputRoute {
                device_id: format!("device-{index}"),
                ..OutputRoute::default()
            })
            .collect();

        let routes = normalize_routes(routes);

        assert_eq!(routes.len(), MAX_OUTPUT_ROUTES);
        assert_eq!(routes[0].device_id, "default");
        assert!(!routes[0].enabled);
        assert_eq!(routes[MAX_OUTPUT_ROUTES - 1].device_id, "device-6");
    }

    #[test]
    fn output_routes_normalize_labels_and_keep_first_device_id() {
        let routes = normalize_routes(vec![
            OutputRoute {
                device_id: " speaker ".into(),
                label: "音".repeat(70),
                volume: 1.5,
                ..OutputRoute::default()
            },
            OutputRoute {
                device_id: "speaker".into(),
                label: "另一个标签".into(),
                ..OutputRoute::default()
            },
            OutputRoute {
                device_id: "headphones".into(),
                label: " ".into(),
                volume: -1.,
                ..OutputRoute::default()
            },
        ]);

        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].device_id, "default");
        assert!(!routes[0].enabled);
        assert_eq!(routes[1].device_id, "speaker");
        assert_eq!(routes[1].label, "音".repeat(64));
        assert_eq!(routes[1].volume, 1.);
        assert_eq!(routes[2].label, "输出设备");
        assert_eq!(routes[2].volume, 0.);
    }

    #[test]
    fn output_routes_preserve_the_explicit_default_and_fill_missing_labels() {
        assert_eq!(normalize_routes(vec![]), [OutputRoute::default()]);

        let fields = json!({"outputRoutes": [
            {"deviceId": "headphones"},
            {"deviceId": "default", "enabled": true}
        ]});
        let prefs = decode_prefs(fields.as_object().unwrap().clone()).unwrap();
        let routes = normalize_prefs(prefs, &PLAYBACK_RATES).output_routes;
        assert_eq!(routes[0].device_id, "default");
        assert!(routes[0].enabled);
        assert_eq!(routes[1].label, "输出设备");
    }
}
