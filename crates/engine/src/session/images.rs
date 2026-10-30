//! UX-5 en la sesión: cuándo se cargan solas las imágenes remotas de un correo, la lista de
//! remitentes y dominios permitidos (Ajustes → Privacidad) y la caché de descargas. La lógica
//! pura vive en [`crate::remote_images`].

use std::sync::Arc;

use iuredesk_model::MessageId;

use super::Session;
use crate::lang::Text;
use crate::remote_images::{self, ImageCache, RemoteImages};

impl Session {
    /// Caché de imágenes remotas (con disco solo si la sesión guarda preferencias: las pruebas y
    /// el ejemplo no escriben en la caché del usuario).
    pub(crate) fn image_cache(&mut self) -> Arc<ImageCache> {
        if let Some(c) = &self.image_cache {
            return Arc::clone(c);
        }
        let dir = self.prefs_path.is_some().then(remote_images::cache_root);
        let c = Arc::new(ImageCache::new(dir));
        self.image_cache = Some(Arc::clone(&c));
        c
    }

    /// Remitente de un mensaje (para el aviso de imágenes): dirección y, si se ofrece, dominio
    /// («Mostrar siempre de este dominio» no se ofrece para Gmail, Outlook…).
    pub fn image_sender(&mut self, id: MessageId) -> Option<(String, Option<String>)> {
        let addr = self.envelope(id)?.from.first().map(|m| m.addr.clone())?;
        let a = remote_images::normalize_sender(&addr)?;
        let d = remote_images::domain_offered(&a);
        Some((a, d))
    }

    /// ¿El remitente es de confianza? Un contacto, una dirección propia o alguien con relación en
    /// Iurefficient.
    fn image_trusted(&mut self, id: MessageId, addr: &str) -> bool {
        if self.my_addresses().iter().any(|a| a.eq_ignore_ascii_case(addr)) {
            return true;
        }
        if self.contacts().iter().any(|c| c.emails.iter().any(|e| e.value.eq_ignore_ascii_case(addr))) {
            return true;
        }
        self.iure_badge(id).is_some()
    }

    /// ¿Se cargan solas las imágenes remotas de este mensaje? (modo de Ajustes → Privacidad y
    /// lista de permitidos; los píxeles de rastreo se bloquean igual al descargar).
    pub fn remote_images_auto(&mut self, id: MessageId) -> bool {
        let Some((addr, _)) = self.image_sender(id) else { return false };
        let mode = self.prefs.remote_images;
        let lists = (&self.prefs.image_senders, &self.prefs.image_domains);
        if remote_images::allowed_by_list(lists.0, lists.1, &addr) {
            return true;
        }
        let trusted = mode == RemoteImages::Trusted && self.image_trusted(id, &addr);
        remote_images::should_load(mode, &[], &[], &addr, trusted)
    }

    /// Modo de imágenes remotas.
    pub fn set_remote_images(&mut self, mode: RemoteImages) {
        if self.prefs.remote_images != mode {
            self.prefs.remote_images = mode;
            self.save_prefs();
        }
    }

    /// «Mostrar siempre de este remitente».
    pub fn allow_images_sender(&mut self, addr: &str) -> Option<Text> {
        let a = remote_images::normalize_sender(addr)?;
        remote_images::add_to(&mut self.prefs.image_senders, a.clone());
        self.save_prefs();
        Some(Text::new(
            format!("Images from {a} will always be shown."),
            format!("Las imágenes de {a} se mostrarán siempre."),
        ))
    }

    /// «Mostrar siempre de este dominio».
    pub fn allow_images_domain(&mut self, domain: &str) -> Option<Text> {
        let d = remote_images::normalize_domain(domain)?;
        remote_images::add_to(&mut self.prefs.image_domains, d.clone());
        self.save_prefs();
        Some(Text::new(
            format!("Images from {d} will always be shown."),
            format!("Las imágenes de {d} se mostrarán siempre."),
        ))
    }

    /// Quita un remitente de la lista de permitidos.
    pub fn remove_image_sender(&mut self, addr: &str) {
        self.prefs.image_senders.retain(|x| !x.eq_ignore_ascii_case(addr));
        self.save_prefs();
    }

    /// Quita un dominio de la lista de permitidos.
    pub fn remove_image_domain(&mut self, domain: &str) {
        self.prefs.image_domains.retain(|x| !x.eq_ignore_ascii_case(domain));
        self.save_prefs();
    }
}
