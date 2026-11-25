/* Dino.notes — notas de parche dentro del launcher.

   Recibe `PatchNotes` (ya saneadas por el núcleo) y construye el DOM **solo con
   createElement / textContent**: nada de innerHTML, así que un catálogo con marcado no
   puede inyectar nada. El formato inline admite `**negrita**`, `*cursiva*` y `` `código` ``.

   Las imágenes se piden con `no-referrer`, se cargan perezosas, y las que fallen se quitan
   en vez de dejar un hueco roto. Pulsar una la abre grande, sin salir del launcher. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});

  var KINDS = {
    feature: { label: "Novedades",    cls: "k-feature" },
    change:  { label: "Cambios",      cls: "k-change" },
    fix:     { label: "Correcciones", cls: "k-fix" },
    note:    { label: "Aviso",        cls: "k-note" }
  };

  function el(tag, cls, text){
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  /* **x** → <strong>, *x* → <em>, `x` → <code>. Lo que no cierre, queda como texto. */
  var INLINE = /(\*\*[^*\n]+\*\*|\*[^*\n]+\*|`[^`\n]+`)/g;
  function inline(parent, text){
    String(text).split(INLINE).forEach(function(part){
      if (!part) return;
      var m;
      if ((m = /^\*\*([^*\n]+)\*\*$/.exec(part))) parent.appendChild(el("strong", null, m[1]));
      else if ((m = /^\*([^*\n]+)\*$/.exec(part))) parent.appendChild(el("em", null, m[1]));
      else if ((m = /^`([^`\n]+)`$/.exec(part))) parent.appendChild(el("code", null, m[1]));
      else parent.appendChild(document.createTextNode(part));
    });
    return parent;
  }

  var lightbox = null;
  function zoom(src, caption){
    if (!lightbox) {
      lightbox = el("div", "lightbox");
      lightbox.hidden = true;
      lightbox.addEventListener("click", function(){ lightbox.hidden = true; lightbox.textContent = ""; });
      document.body.appendChild(lightbox);
      document.addEventListener("keydown", function(e){
        if (e.key === "Escape" && !lightbox.hidden) { lightbox.hidden = true; lightbox.textContent = ""; e.stopPropagation(); }
      }, true);
    }
    lightbox.textContent = "";
    var img = el("img");
    img.referrerPolicy = "no-referrer";
    img.alt = caption || "";
    img.src = src;
    lightbox.appendChild(img);
    if (caption) lightbox.appendChild(el("div", "lightbox-cap", caption));
    lightbox.hidden = false;
  }

  function figure(i){
    var f = el("figure", "notes-fig");
    var img = el("img");
    img.loading = "lazy";
    img.decoding = "async";
    img.referrerPolicy = "no-referrer";
    img.alt = i.caption || "";
    img.addEventListener("error", function(){ f.remove(); });
    img.addEventListener("click", function(){ zoom(i.url, i.caption); });
    img.src = i.url;
    f.appendChild(img);
    if (i.caption) f.appendChild(el("figcaption", null, i.caption));
    return f;
  }

  /* Pinta `notes` dentro de `box` (lo vacía antes). */
  function render(box, notes){
    box.textContent = "";
    if (!notes) return;
    if (notes.title) box.appendChild(el("h3", "notes-title", notes.title));
    if (notes.summary) box.appendChild(inline(el("p", "notes-summary"), notes.summary));
    (notes.sections || []).forEach(function(s){
      var k = KINDS[s.kind] || KINDS.change;
      var sec = el("section", "notes-sec " + k.cls);
      var head = el("div", "notes-sec-head");
      head.appendChild(el("span", "notes-tag", k.label));
      if (s.title) head.appendChild(el("span", "notes-sec-title", s.title));
      sec.appendChild(head);
      var ul = el("ul", "notes-list");
      (s.items || []).forEach(function(t){ ul.appendChild(inline(el("li"), t)); });
      sec.appendChild(ul);
      box.appendChild(sec);
    });
    if (notes.images && notes.images.length) {
      var g = el("div", "notes-gallery");
      notes.images.forEach(function(i){ g.appendChild(figure(i)); });
      box.appendChild(g);
    }
  }

  D.notes = { render: render, inline: inline };
})();
