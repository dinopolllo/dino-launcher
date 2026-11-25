/* Dino.pubextras — lo opcional de un pack en el editor de publicación: la actividad de
   Discord, las notas de parche y el tema.

   Es un formulario autónomo: `mount(host)` lo construye dentro de `host`, `read()` devuelve
   el objeto `extras` que espera `admin_publish` (`{presence, notes, theme}`; una sección
   apagada o vacía no se manda, y un pack sin nada publica exactamente lo de siempre) y
   `write(pack)` lo rellena con lo que el pack ya tenía publicado, para actualizarlo.

   Los archivos que se eligen en el equipo (logo, imágenes de las notas, banner, sonidos) se
   quedan como `data:` en el formulario; el núcleo los sube al repo al publicar y los cambia
   por su enlace. Aquí solo se comprueba el tipo y el tamaño para avisar antes de esperar. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});

  var MAX_IMG = 2000000, MAX_SND = 400000;
  var FRAMES = [["", "Sin marco"], ["glow", "Resplandor"], ["double", "Doble línea"], ["pixel", "Píxel"]];
  var KINDS = [["feature", "Novedades"], ["change", "Cambios"], ["fix", "Correcciones"], ["note", "Aviso"]];
  var PRESETS = ["soft", "blip", "chime", "void"];
  var SOUNDS = [["click", "Clic"], ["hover", "Pasar por encima"], ["done", "Instalación lista"], ["play", "Pulsar Jugar"]];
  var COLORS = [["primary", "Principal"], ["accent", "Resalte"], ["deep", "Oscuro"], ["ink", "Fondo"],
                ["surface", "Superficies"], ["border", "Bordes"], ["text", "Texto"]];

  var ui = {};                  /* referencias a los controles, por nombre */
  var sections = [];            /* filas de las notas: { kind, title, items } */
  var images = [];              /* filas de imágenes: { url, caption } */
  var onChange = function(){};

  function el(tag, cls, text){
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function field(label, control, note){
    var f = el("div", "field");
    f.appendChild(el("label", null, label));
    f.appendChild(control);
    if (note) f.appendChild(el("div", "field-note", note));
    return f;
  }
  function text(ph, max){
    var i = el("input"); i.type = "text"; i.spellcheck = false; i.autocomplete = "off";
    if (ph) i.placeholder = ph;
    if (max) i.maxLength = max;
    i.addEventListener("input", function(){ onChange(); });
    return i;
  }
  function toggle(title, sub, on){
    var row = el("div", "field-switch");
    var left = el("div");
    left.appendChild(el("div", "fs-t", title));
    if (sub) left.appendChild(el("div", "fs-s", sub));
    var sw = el("button", "switch");
    sw.type = "button"; sw.setAttribute("role", "switch");
    sw.setAttribute("aria-checked", on ? "true" : "false"); sw.setAttribute("aria-label", title);
    sw.appendChild(el("b"));
    sw.addEventListener("click", function(){
      sw.setAttribute("aria-checked", sw.getAttribute("aria-checked") === "true" ? "false" : "true");
      onChange();
    });
    row.appendChild(left); row.appendChild(sw);
    row.sw = sw;
    return row;
  }
  function isOn(row){ return row.sw.getAttribute("aria-checked") === "true"; }
  function setOn(row, v){ row.sw.setAttribute("aria-checked", v ? "true" : "false"); }
  function btn(label, cls, fn){
    var b = el("button", cls || "btn", label);
    b.type = "button";
    b.addEventListener("click", fn);
    return b;
  }
  function group(title, hint){
    var d = el("details", "pub-extra");
    var s = el("summary");
    s.appendChild(el("span", "pub-extra-t", title));
    if (hint) s.appendChild(el("span", "pub-extra-h", hint));
    d.appendChild(s);
    var body = el("div", "pub-extra-body");
    d.appendChild(body);
    d.body = body;
    return d;
  }

  /* Elige un archivo del equipo y lo devuelve como `data:`, tras comprobar tipo y peso. */
  function pick(accept, max, done){
    var inp = document.createElement("input");
    inp.type = "file"; inp.accept = accept;
    inp.addEventListener("change", function(){
      var f = inp.files && inp.files[0];
      if (!f) return;
      if (f.size > max) { done(null, "«" + f.name + "» pesa " + Math.round(f.size / 1000) + " KB; el máximo es " + Math.round(max / 1000) + " KB."); return; }
      var fr = new FileReader();
      fr.onload = function(){ done({ data: String(fr.result), name: f.name }); };
      fr.onerror = function(){ done(null, "No pude leer «" + f.name + "»."); };
      fr.readAsDataURL(f);
    });
    inp.click();
  }

  /* Un control de «enlace o archivo»: texto con la URL y un botón para subir uno. */
  function media(label, accept, max, ph, note){
    var wrap = el("div", "pub-media");
    var input = text(ph);
    var status = el("div", "field-note");
    var row = el("div", "pub-media-row");
    row.appendChild(input);
    row.appendChild(btn("Archivo…", "btn", function(){
      pick(accept, max, function(r, err){
        if (err) { status.textContent = err; return; }
        input.value = r.data; input.dataset.file = r.name;
        show(); onChange();
      });
    }));
    function show(){
      var v = input.value;
      status.textContent = v.indexOf("data:") === 0 ? "Se subirá al publicar: " + (input.dataset.file || "archivo") : "";
      /* Un `data:` entero en la caja es ruido: se muestra el nombre del archivo. */
      input.title = v.indexOf("data:") === 0 ? input.dataset.file || "" : "";
    }
    input.addEventListener("input", function(){ if (input.value.indexOf("data:") !== 0) delete input.dataset.file; show(); });
    wrap.appendChild(row); wrap.appendChild(status);
    wrap.input = input; wrap.refresh = show;
    var f = field(label, wrap, note);
    f.media = wrap;
    return f;
  }
  function mediaValue(m){ return (m.media.input.value || "").trim(); }
  function mediaSet(m, v){
    m.media.input.value = v || "";
    if (v && v.indexOf("data:") === 0) m.media.input.dataset.file = "archivo";
    m.media.refresh();
  }

  /* ------------------------------------------------------------ Discord */
  function buildPresence(){
    var g = group("Discord", "actividad mientras se juega");
    ui.presOn = toggle("Mostrar la actividad en Discord", "si está apagado no se manda nada", false);
    ui.presDetails = text("Jugando {pack} en Minecraft", 128);
    ui.presState = text("{server}", 128);
    ui.presImage = media("Logo", "image/png,image/jpeg,image/webp,image/gif", MAX_IMG,
      "https://… o archivo", "cuadrado, mínimo 512×512; o la clave de un asset de tu aplicación de Discord");
    ui.presImageText = text("Texto al pasar por el logo", 128);
    ui.presClient = text("opcional: id de tu aplicación de Discord", 20);
    ui.presJoinOn = toggle("Botón para unirse", "un botón en la actividad que lleva a la invitación", false);
    ui.presJoinLabel = text("Unirse con Dino Launcher", 32);
    ui.presJoinUrl = text("https://… (vacío: la invitación del pack)");
    g.body.appendChild(ui.presOn);
    g.body.appendChild(field("Detalle", ui.presDetails,
      "Puedes usar {pack}, {server}, {version}, {minecraft} y {player}"));
    g.body.appendChild(field("Estado", ui.presState));
    g.body.appendChild(ui.presImage);
    g.body.appendChild(field("Texto del logo", ui.presImageText));
    g.body.appendChild(field("Aplicación de Discord", ui.presClient,
      "con otro id el logo y el nombre son los de esa aplicación"));
    g.body.appendChild(ui.presJoinOn);
    g.body.appendChild(field("Texto del botón", ui.presJoinLabel));
    g.body.appendChild(field("Enlace del botón", ui.presJoinUrl));
    return g;
  }
  function readPresence(){
    if (!isOn(ui.presOn)) return null;
    var p = { enabled: true };
    var v;
    if ((v = ui.presDetails.value.trim())) p.details = v;
    if ((v = ui.presState.value.trim())) p.state = v;
    if ((v = mediaValue(ui.presImage))) p.image = v;
    if ((v = ui.presImageText.value.trim())) p.image_text = v;
    if ((v = ui.presClient.value.trim())) p.client_id = v;
    if (isOn(ui.presJoinOn) && (v = ui.presJoinLabel.value.trim())) {
      p.join = { label: v };
      if ((v = ui.presJoinUrl.value.trim())) p.join.url = v;
    }
    return p;
  }
  function writePresence(p){
    p = p || {};
    setOn(ui.presOn, !!p.enabled);
    ui.presDetails.value = p.details || "";
    ui.presState.value = p.state || "";
    mediaSet(ui.presImage, p.image);
    ui.presImageText.value = p.image_text || "";
    ui.presClient.value = p.client_id || "";
    setOn(ui.presJoinOn, !!p.join);
    ui.presJoinLabel.value = (p.join && p.join.label) || "";
    ui.presJoinUrl.value = (p.join && p.join.url) || "";
  }

  /* ------------------------------------------------------------ notas */
  function sectionRow(s){
    var row = el("div", "pub-sec");
    var top = el("div", "pub-sec-top");
    var kind = el("select");
    KINDS.forEach(function(k){ var o = el("option", null, k[1]); o.value = k[0]; kind.appendChild(o); });
    kind.value = s.kind || "change";
    var title = text("Título (opcional)", 80); title.value = s.title || "";
    var del = btn("Quitar", "link-min", function(){
      row.remove(); sections.splice(sections.indexOf(s), 1); onChange();
    });
    top.appendChild(kind); top.appendChild(title); top.appendChild(del);
    var items = el("textarea");
    items.rows = 4; items.spellcheck = true;
    items.placeholder = "Una línea por cambio. Admite **negrita**, *cursiva* y `código`.";
    items.value = (s.items || []).join("\n");
    items.addEventListener("input", function(){ onChange(); });
    kind.addEventListener("change", function(){ onChange(); });
    row.appendChild(top); row.appendChild(items);
    s.kindEl = kind; s.titleEl = title; s.itemsEl = items;
    return row;
  }
  function addSection(s){
    s = s || { kind: "feature" };
    sections.push(s);
    ui.secList.appendChild(sectionRow(s));
  }
  function imageRow(i){
    var row = el("div", "pub-sec");
    var m = media("Imagen", "image/png,image/jpeg,image/webp,image/gif", MAX_IMG, "https://… o archivo");
    mediaSet(m, i.url);
    var cap = text("Pie de la imagen (opcional)", 160); cap.value = i.caption || "";
    var del = btn("Quitar", "link-min", function(){
      row.remove(); images.splice(images.indexOf(i), 1); onChange();
    });
    row.appendChild(m); row.appendChild(cap); row.appendChild(del);
    i.mediaEl = m; i.capEl = cap;
    return row;
  }
  function addImage(i){
    if (images.length >= 8) return;
    i = i || {};
    images.push(i);
    ui.imgList.appendChild(imageRow(i));
  }
  function buildNotes(){
    var g = group("Notas de parche", "se leen en el launcher al actualizar");
    ui.notesTitle = text("Novedades de la 3.16", 120);
    ui.notesSummary = el("textarea");
    ui.notesSummary.rows = 2;
    ui.notesSummary.placeholder = "Un resumen corto (opcional)";
    ui.notesSummary.addEventListener("input", function(){ onChange(); });
    ui.secList = el("div", "pub-sec-list");
    ui.imgList = el("div", "pub-sec-list");
    g.body.appendChild(field("Título", ui.notesTitle));
    g.body.appendChild(field("Resumen", ui.notesSummary));
    g.body.appendChild(el("div", "pub-sub", "Secciones"));
    g.body.appendChild(ui.secList);
    g.body.appendChild(btn("Añadir sección", "btn", function(){ addSection(); onChange(); }));
    g.body.appendChild(el("div", "pub-sub", "Imágenes"));
    g.body.appendChild(ui.imgList);
    g.body.appendChild(btn("Añadir imagen", "btn", function(){ addImage(); onChange(); }));
    return g;
  }
  function readNotes(){
    var n = {};
    var v;
    if ((v = ui.notesTitle.value.trim())) n.title = v;
    if ((v = ui.notesSummary.value.trim())) n.summary = v;
    var secs = [];
    sections.forEach(function(s){
      var items = s.itemsEl.value.split("\n").map(function(t){ return t.trim(); }).filter(Boolean);
      if (!items.length) return;
      var o = { kind: s.kindEl.value, items: items };
      if (s.titleEl.value.trim()) o.title = s.titleEl.value.trim();
      secs.push(o);
    });
    if (secs.length) n.sections = secs;
    var imgs = [];
    images.forEach(function(i){
      var url = mediaValue(i.mediaEl);
      if (!url) return;
      var o = { url: url };
      if (i.capEl.value.trim()) o.caption = i.capEl.value.trim();
      imgs.push(o);
    });
    if (imgs.length) n.images = imgs;
    return Object.keys(n).length ? n : null;
  }
  function writeNotes(n){
    n = n || {};
    ui.notesTitle.value = n.title || "";
    ui.notesSummary.value = n.summary || "";
    ui.secList.textContent = ""; ui.imgList.textContent = "";
    sections = []; images = [];
    (n.sections || []).forEach(function(s){ addSection({ kind: s.kind, title: s.title, items: s.items }); });
    (n.images || []).forEach(function(i){ addImage({ url: i.url, caption: i.caption }); });
  }

  /* ------------------------------------------------------------ tema */
  function colorRow(key, label){
    var row = el("div", "pub-color");
    var t = text("#rrggbb", 7);
    var p = el("input"); p.type = "color"; p.value = "#73bf69"; p.className = "pub-swatch";
    p.addEventListener("input", function(){ t.value = p.value; onChange(); });
    t.addEventListener("input", function(){ if (/^#[0-9a-f]{6}$/i.test(t.value)) p.value = t.value; });
    row.appendChild(el("span", "pub-color-k", label));
    row.appendChild(t); row.appendChild(p);
    ui["col_" + key] = t;
    return row;
  }
  function soundRow(key, label){
    var row = el("div", "pub-snd");
    var sel = el("select");
    var none = el("option", null, "Silencio"); none.value = ""; sel.appendChild(none);
    PRESETS.forEach(function(p){ var o = el("option", null, "De fábrica: " + p); o.value = p; sel.appendChild(o); });
    var custom = el("option", null, "Mi archivo o enlace"); custom.value = "_custom"; sel.appendChild(custom);
    var m = media("Archivo", "audio/ogg,audio/mpeg,audio/wav", MAX_SND, "https://… o archivo");
    m.hidden = true;
    sel.addEventListener("change", function(){ m.hidden = sel.value !== "_custom"; onChange(); });
    var test = btn("Probar", "link-min", function(){
      var v = sel.value === "_custom" ? mediaValue(m) : sel.value;
      if (v && D.sfx) D.sfx.preview(v.indexOf("data:") === 0 ? "" : v, 0.8);
    });
    var top = el("div", "pub-snd-top");
    top.appendChild(el("span", "pub-color-k", label)); top.appendChild(sel); top.appendChild(test);
    row.appendChild(top); row.appendChild(m);
    ui["snd_" + key] = { sel: sel, media: m };
    return row;
  }
  function buildTheme(){
    var g = group("Tema del launcher", "colores, sonidos y adornos del pack");
    ui.themeOn = toggle("Cambiar el aspecto con este pack", "el jugador puede apagarlo en su perfil", false);
    g.body.appendChild(ui.themeOn);
    g.body.appendChild(el("div", "pub-sub", "Colores (en blanco: los de Dino)"));
    COLORS.forEach(function(c){ g.body.appendChild(colorRow(c[0], c[1])); });
    g.body.appendChild(el("div", "pub-sub", "Sonidos (por omisión no suena nada)"));
    SOUNDS.forEach(function(s){ g.body.appendChild(soundRow(s[0], s[1])); });
    ui.sndVol = el("input"); ui.sndVol.type = "range"; ui.sndVol.min = "0"; ui.sndVol.max = "100"; ui.sndVol.value = "80";
    ui.sndVol.addEventListener("input", function(){ onChange(); });
    g.body.appendChild(field("Volumen del pack", ui.sndVol, "el jugador tiene su propio volumen por encima"));
    g.body.appendChild(el("div", "pub-sub", "Adornos"));
    ui.decBanner = media("Imagen de cabecera", "image/png,image/jpeg,image/webp", MAX_IMG, "https://… o archivo");
    ui.decBadge = text("Servidor oficial", 28);
    ui.decTagline = text("Una frase bajo el nombre", 90);
    ui.decFrame = el("select");
    FRAMES.forEach(function(f){ var o = el("option", null, f[1]); o.value = f[0]; ui.decFrame.appendChild(o); });
    ui.decFrame.addEventListener("change", function(){ onChange(); });
    g.body.appendChild(ui.decBanner);
    g.body.appendChild(field("Etiqueta", ui.decBadge));
    g.body.appendChild(field("Frase", ui.decTagline));
    g.body.appendChild(field("Marco de la ficha", ui.decFrame));
    ui.themePreview = btn("Probar el tema en el launcher", "btn", function(){
      var t = readTheme(true);
      if (!t || !D.theme) return;
      D.theme.apply(t, "preview");
      ui.themePreview.textContent = "Volver al tema normal";
      ui.themePreview.onclick = function(){ endPreview(); };
    });
    g.body.appendChild(ui.themePreview);
    return g;
  }
  function endPreview(){
    if (D.theme) D.theme.clear();
    if (ui.themePreview) {
      ui.themePreview.textContent = "Probar el tema en el launcher";
      ui.themePreview.onclick = null;
    }
    if (typeof D.pubextras.afterPreview === "function") D.pubextras.afterPreview();
  }
  function readTheme(forPreview){
    if (!forPreview && !isOn(ui.themeOn)) return null;
    var t = { enabled: true, colors: {}, sounds: {}, decor: {} };
    var any = false;
    COLORS.forEach(function(c){
      var v = ui["col_" + c[0]].value.trim();
      if (/^#[0-9a-f]{3}([0-9a-f]{3})?$/i.test(v)) { t.colors[c[0]] = v; any = true; }
    });
    SOUNDS.forEach(function(s){
      var r = ui["snd_" + s[0]];
      var v = r.sel.value === "_custom" ? mediaValue(r.media) : r.sel.value;
      /* Para probar en vivo, un archivo del equipo (`data:`) no se puede usar todavía. */
      if (v && !(forPreview && v.indexOf("data:") === 0)) { t.sounds[s[0]] = v; any = true; }
    });
    if (any && Object.keys(t.sounds).length) t.sounds.volume = Number(ui.sndVol.value) / 100;
    var b = mediaValue(ui.decBanner);
    if (b && !(forPreview && b.indexOf("data:") === 0)) { t.decor.banner = b; any = true; }
    if (b && forPreview && b.indexOf("data:") === 0) { t.decor.banner = b; }
    if (ui.decBadge.value.trim()) { t.decor.badge = ui.decBadge.value.trim(); any = true; }
    if (ui.decTagline.value.trim()) { t.decor.tagline = ui.decTagline.value.trim(); any = true; }
    if (ui.decFrame.value) { t.decor.frame = ui.decFrame.value; any = true; }
    return any ? t : null;
  }
  function writeTheme(t){
    t = t || {};
    var c = t.colors || {}, s = t.sounds || {}, d = t.decor || {};
    setOn(ui.themeOn, !!t.enabled);
    COLORS.forEach(function(k){
      ui["col_" + k[0]].value = c[k[0]] || "";
      var sw = ui["col_" + k[0]].nextSibling;
      if (sw && c[k[0]]) sw.value = c[k[0]];
    });
    SOUNDS.forEach(function(k){
      var r = ui["snd_" + k[0]], v = s[k[0]] || "";
      var preset = PRESETS.indexOf(v) >= 0;
      r.sel.value = !v ? "" : preset ? v : "_custom";
      mediaSet(r.media, preset ? "" : v);
      r.media.hidden = r.sel.value !== "_custom";
    });
    ui.sndVol.value = String(Math.round((typeof s.volume === "number" ? s.volume : 0.8) * 100));
    mediaSet(ui.decBanner, d.banner);
    ui.decBadge.value = d.badge || "";
    ui.decTagline.value = d.tagline || "";
    ui.decFrame.value = d.frame || "";
  }

  D.pubextras = {
    /* `change` se llama con cada edición (para repintar la vista previa del editor). */
    mount: function(host, change){
      onChange = change || function(){};
      host.textContent = "";
      host.appendChild(buildPresence());
      host.appendChild(buildNotes());
      host.appendChild(buildTheme());
      this.write({});
    },
    /* El objeto que espera `Borrador.extras`; `{}` si el pack no lleva nada. */
    read: function(){
      var x = {}, p = readPresence(), n = readNotes(), t = readTheme(false);
      if (p) x.presence = p;
      if (n) x.notes = n;
      if (t) x.theme = t;
      return x;
    },
    /* Rellena con un `PackRemoto` ya publicado (los extras cuelgan de él). */
    write: function(pack){
      pack = pack || {};
      writePresence(pack.presence);
      writeNotes(pack.notes);
      writeTheme(pack.theme);
      endPreview();
    },
    endPreview: endPreview
  };
})();
