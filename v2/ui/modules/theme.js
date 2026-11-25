/* Dino.theme — el aspecto del launcher según el modpack elegido.

   Lo publica el pack (`theme`, ya saneado por el núcleo) y solo se aplica si el jugador lo
   permite. Es **reversible por construcción**: aplicar solo escribe variables CSS en
   `<html>` y rellena los adornos; quitar borra exactamente lo que se escribió, y el tema
   base vuelve sin recargar nada. Lo que el pack no define se queda como estaba.

   Colores: se pueden dar solo `primary` (el resto se deriva) o todos. El texto se
   comprueba contra el fondo: si no se lee, se descarta el color del texto y no el tema.

   Adornos (no tocan la disposición): imagen de cabecera en la ficha, etiqueta junto al
   nombre, frase bajo el nombre y marco de la ficha. Los sonidos van aparte, en Dino.sfx. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});
  var root = document.documentElement;

  var written = [];               /* nombres de variables que escribió el tema actual */
  var currentKey = null;

  function parse(hex){
    var m = /^#([0-9a-f]{6})$/i.exec(hex || "");
    if (!m) return null;
    var n = parseInt(m[1], 16);
    return [n >> 16, (n >> 8) & 255, n & 255];
  }
  function mix(a, b, t){
    return [0, 1, 2].map(function(i){ return Math.round(a[i] + (b[i] - a[i]) * t); });
  }
  function css(c){ return "rgb(" + c[0] + "," + c[1] + "," + c[2] + ")"; }
  function csv(c){ return c[0] + "," + c[1] + "," + c[2]; }
  function rgba(c, a){ return "rgba(" + csv(c) + "," + a + ")"; }
  function lum(c){
    var f = c.map(function(v){ v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); });
    return 0.2126 * f[0] + 0.7152 * f[1] + 0.0722 * f[2];
  }
  function contrast(a, b){
    var x = lum(a), y = lum(b);
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  }

  var WHITE = [255, 255, 255], BLACK = [0, 0, 0], BASE_INK = [3, 6, 10];

  /* Las variables que le tocan al tema, a partir de los colores publicados. */
  function variables(c){
    var v = {};
    var P = parse(c.primary), I = parse(c.ink), S = parse(c.surface), B = parse(c.border), T = parse(c.text);
    var Dp = parse(c.deep), A = parse(c.accent);
    if (P || Dp || A) {
      P = P || A || Dp;
      Dp = Dp || mix(P, BLACK, 0.62);
      A = A || mix(P, WHITE, 0.25);
      v["--green"] = css(P);          v["--dino-mid"] = css(P);
      v["--green-rgb"] = csv(P);      v["--green-glow"] = rgba(P, 0.5);
      v["--dino-light"] = css(A);
      v["--green-deep"] = css(Dp);    v["--deep-rgb"] = csv(Dp);
      v["--dino-dark"] = css(mix(Dp, BLACK, 0.35));
      v["--green-hot"] = css(mix(P, Dp, 0.45));
    }
    if (I) {
      v["--ink"] = css(I); v["--ink-2"] = css(mix(I, WHITE, 0.03));
      v["--window-tint"] = rgba(I, 0.30);
    }
    if (S) {
      v["--glass-0"] = rgba(S, 0.030); v["--glass-1"] = rgba(S, 0.052);
      v["--glass-2"] = rgba(S, 0.080); v["--glass-3"] = rgba(S, 0.115);
    }
    if (B) {
      v["--hairline"] = rgba(B, 0.07); v["--hairline-strong"] = rgba(B, 0.115);
      v["--specular"] = rgba(B, 0.18);
    }
    if (T && contrast(T, I || BASE_INK) >= 4.5) {
      v["--text"] = css(T);
      v["--text-dim"] = css(mix(T, I || BASE_INK, 0.45));
      v["--text-faint"] = css(mix(T, I || BASE_INK, 0.65));
    }
    return v;
  }

  function clearVars(){
    written.forEach(function(n){ root.style.removeProperty(n); });
    written = [];
  }

  var URL_OK = /^https:\/\/[^\s"'<>\\]+$/;

  /* Los adornos viven en elementos que ya están en la página (ids `deco*`); cada uno se
     esconde si el tema no lo usa. Todo con textContent / estilo, nunca con innerHTML. */
  function decor(d){
    var hero = document.getElementById("heroDeco");
    var badge = document.getElementById("packBadge");
    var tag = document.getElementById("packTagline");
    var card = document.querySelector(".hero");
    d = d || {};
    if (hero) {
      var ok = d.banner && URL_OK.test(d.banner);
      hero.hidden = !ok;
      hero.style.backgroundImage = ok ? 'url("' + d.banner + '")' : "";
    }
    if (badge) { badge.hidden = !d.badge; badge.textContent = d.badge || ""; }
    if (tag)   { tag.hidden = !d.tagline; tag.textContent = d.tagline || ""; }
    if (card) {
      if (d.frame && d.frame !== "none") card.setAttribute("data-frame", d.frame);
      else card.removeAttribute("data-frame");
    }
  }

  function apply(theme, key){
    var k = theme && theme.enabled ? (key || "x") + ":" + JSON.stringify(theme) : null;
    if (k === currentKey) return;               /* nada cambió: no se repinta */
    currentKey = k;
    clearVars();
    if (!k) {
      root.removeAttribute("data-pack-theme");
      decor(null);
      if (D.sfx) D.sfx.setTheme(null);
      return;
    }
    var vars = variables(theme.colors || {});
    Object.keys(vars).forEach(function(n){ root.style.setProperty(n, vars[n]); written.push(n); });
    root.setAttribute("data-pack-theme", key || "x");
    decor(theme.decor);
    if (D.sfx) D.sfx.setTheme(theme.sounds || null);
  }

  D.theme = {
    /* `theme` es el de `InstanceView.theme` (o null para el tema base). `key` identifica al
       pack, solo para distinguir un tema de otro en `data-pack-theme`. */
    apply: apply,
    clear: function(){ apply(null); },
    /* Los colores que el editor de publicación está probando, sin guardar nada. */
    contrast: contrast, parse: parse
  };
})();
