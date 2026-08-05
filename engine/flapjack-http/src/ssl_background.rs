use std::path::Path;

/// SSL configuration projected into the background-task planning seam.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfiguredSsl<'a, Manager> {
    pub(crate) manager: Option<&'a Manager>,
    pub(crate) material_dir: &'a Path,
}

/// Independent renewal and material-observation tasks selected at startup.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SslBackgroundPlan<'a, Manager, TlsResolver> {
    pub(crate) renewal_manager: Option<&'a Manager>,
    pub(crate) material_observer: Option<MaterialObserverPlan<'a, Manager, TlsResolver>>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MaterialObserverPlan<'a, Manager, TlsResolver> {
    pub(crate) resolver: &'a TlsResolver,
    pub(crate) material_dir: &'a Path,
    /// Expiry reporting is optional and does not gate material observation.
    pub(crate) expiry_manager: Option<&'a Manager>,
}

pub(crate) fn ssl_background_plan<'a, Manager, TlsResolver>(
    configured_ssl: Option<ConfiguredSsl<'a, Manager>>,
    tls_resolver: Option<&'a TlsResolver>,
) -> SslBackgroundPlan<'a, Manager, TlsResolver> {
    let Some(configured_ssl) = configured_ssl else {
        return SslBackgroundPlan {
            renewal_manager: None,
            material_observer: None,
        };
    };
    SslBackgroundPlan {
        renewal_manager: configured_ssl.manager,
        material_observer: tls_resolver.map(|resolver| MaterialObserverPlan {
            resolver,
            material_dir: configured_ssl.material_dir,
            expiry_manager: configured_ssl.manager,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{ssl_background_plan, ConfiguredSsl};

    #[test]
    fn plan_covers_renewal_and_material_observer_gates() {
        let manager = "manager";
        let resolver = "resolver";
        let material_dir = std::path::Path::new("/material/acme");
        let with_manager = ConfiguredSsl {
            manager: Some(&manager),
            material_dir,
        };
        let without_manager = ConfiguredSsl {
            manager: None::<&&str>,
            material_dir,
        };

        let unconfigured = ssl_background_plan(None::<ConfiguredSsl<'_, &str>>, Some(&resolver));
        assert!(unconfigured.renewal_manager.is_none());
        assert!(unconfigured.material_observer.is_none());

        let renewal_only = ssl_background_plan(Some(with_manager), None::<&&str>);
        assert_eq!(renewal_only.renewal_manager.copied(), Some(manager));
        assert!(renewal_only.material_observer.is_none());

        let observer_only = ssl_background_plan(Some(without_manager), Some(&resolver));
        assert!(observer_only.renewal_manager.is_none());
        let observer = observer_only
            .material_observer
            .expect("configured material plus a TLS resolver must start the observer");
        assert_eq!(*observer.resolver, resolver);
        assert_eq!(observer.material_dir, material_dir);
        assert!(observer.expiry_manager.is_none());

        let both_owners = ssl_background_plan(Some(with_manager), Some(&resolver));
        assert_eq!(both_owners.renewal_manager.copied(), Some(manager));
        let observer = both_owners
            .material_observer
            .expect("both owners present must start the observer");
        assert_eq!(*observer.resolver, resolver);
        assert_eq!(observer.material_dir, material_dir);
        assert_eq!(observer.expiry_manager.copied(), Some(manager));

        let material_only = ssl_background_plan(Some(without_manager), None::<&&str>);
        assert!(material_only.renewal_manager.is_none());
        assert!(material_only.material_observer.is_none());
    }
}
