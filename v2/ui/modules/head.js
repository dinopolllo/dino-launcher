/* Dino.head — la cabeza de la cuenta, que mira al cursor como el muñeco del inventario de
   Minecraft.

   Es un cubo CSS de seis caras cortadas de la skin (cara, capa del sombrero, laterales,
   nuca y coronilla). Sin skin propia se usan la cara dibujada y colores lisos para el resto.

   Rendimiento:
     · no hay bucle permanente: `requestAnimationFrame` solo corre mientras la cabeza se
       está moviendo, y se detiene en cuanto llega a su sitio;
     · solo se escucha el puntero mientras la pieza está activa; `Dino.perf` la pausa con
       la ventana sin foco, al abrir el juego y durante la partida;
     · `Dino.head.setEnabled(false)` la desmonta del todo y vuelve el canvas plano. */
(function(){
  "use strict";
  var D = (window.Dino = window.Dino || {});

  var SIZE = 30;                 /* px de la cabeza, igual que el canvas plano de antes */
  var MAX_YAW = 38, MAX_PITCH = 24;   /* grados: lo justo para que se note sin marear */
  var SPAN = 260;                /* distancia (px) a la que el giro llega a su tope */
  var TAU = 0.085;               /* segundos: constante de tiempo del suavizado */
  var EPS = 0.04;                /* grados: por debajo, ya está en su sitio */

  /* Dónde está cada cara en una skin de 64×64 (x, y, ancho 8): base y capa del sombrero. */
  var UV = {
    front:  [[8, 8],  [40, 8]],
    back:   [[24, 8], [56, 8]],
    /* Con la cabeza de frente, la cara de la derecha de la pantalla es el LADO IZQUIERDO
       del jugador (y al revés). */
    right:  [[16, 8], [48, 8]],
    left:   [[0, 8],  [32, 8]],
    top:    [[8, 0],  [40, 0]],
    bottom: [[16, 0], [48, 0]]
  };
  var TRANSFORM = {
    front:  "translateZ(" + SIZE / 2 + "px)",
    back:   "rotateY(180deg) translateZ(" + SIZE / 2 + "px)",
    right:  "rotateY(90deg) translateZ(" + SIZE / 2 + "px)",
    left:   "rotateY(-90deg) translateZ(" + SIZE / 2 + "px)",
    top:    "rotateX(90deg) translateZ(" + SIZE / 2 + "px)",
    bottom: "rotateX(-90deg) translateZ(" + SIZE / 2 + "px)"
  };

  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  var host = null, cube = null, flat = null, faces = {};
  var wanted = true;             /* la preferencia del jugador */
  var live = false;              /* escuchando el puntero ahora mismo */
  var raf = 0, last = 0;
  var cur = { yaw: 0, pitch: 0 };
  var ptr = null;                /* última posición del puntero, o null si salió de la ventana */

  function faceCanvas(name){
    var c = document.createElement("canvas");
    c.width = 8; c.height = 8;
    c.className = "face " + name;
    c.style.transform = TRANSFORM[name];
    return c;
  }

  /* Pinta la cabeza. `spec`:
       skin   → HTMLImageElement ya cargada (64×64 o más), o null
       flat   → canvas 8×8 con la cara dibujada (de relleno si no hay skin)
       hair, tone → colores CSS para las caras que sin skin no se conocen */
  function paint(spec){
    if (!cube) return;
    Object.keys(UV).forEach(function(n){
      var ctx = faces[n].getContext("2d");
      ctx.imageSmoothingEnabled = false;
      ctx.clearRect(0, 0, 8, 8);
      if (spec.skin && spec.skin.naturalWidth >= 64) {
        var s = spec.skin.naturalWidth / 64;      /* skins HD: la misma retícula a otra escala */
        UV[n].forEach(function(p){
          ctx.drawImage(spec.skin, p[0] * s, p[1] * s, 8 * s, 8 * s, 0, 0, 8, 8);
        });
        return;
      }
      if (n === "front" && spec.flat) { ctx.drawImage(spec.flat, 0, 0, 8, 8); return; }
      ctx.fillStyle = (n === "top" || n === "back") ? spec.hair : spec.tone;
      ctx.fillRect(0, 0, 8, 8);
      if (n === "left" || n === "right") { ctx.fillStyle = spec.hair; ctx.fillRect(0, 0, 8, 3); }
      if (n === "bottom") { ctx.fillStyle = "rgba(0,0,0,.28)"; ctx.fillRect(0, 0, 8, 8); }
    });
  }

  function show(on){
    if (!host) return;
    host.hidden = !on;
    if (flat) flat.hidden = on;
  }

  function setPose(yaw, pitch){
    cur.yaw = yaw; cur.pitch = pitch;
    cube.style.transform = "rotateX(" + pitch.toFixed(2) + "deg) rotateY(" + yaw.toFixed(2) + "deg)";
  }

  /* Un empujón suave: el objetivo crece rápido cerca de la cabeza y se aplana al alejarse,
     así no hay tirones al cruzar la ventana. */
  function soft(v){ return Math.tanh(v * 1.35); }

  function tick(t){
    raf = 0;
    if (!live || !cube) return;
    var dt = last ? Math.min(0.05, (t - last) / 1000) : 0.016;
    last = t;
    var ty = 0, tp = 0;
    if (ptr) {
      var r = host.getBoundingClientRect();
      var dx = (ptr.x - (r.left + r.width / 2)) / SPAN;
      var dy = (ptr.y - (r.top + r.height / 2)) / SPAN;
      ty = soft(dx) * MAX_YAW;
      tp = -soft(dy) * MAX_PITCH;
    }
    var k = 1 - Math.exp(-dt / TAU);
    var yaw = cur.yaw + (ty - cur.yaw) * k;
    var pitch = cur.pitch + (tp - cur.pitch) * k;
    if (Math.abs(ty - yaw) < EPS && Math.abs(tp - pitch) < EPS) {
      setPose(ty, tp);       /* llegó: el bucle se detiene hasta el próximo movimiento */
      last = 0;
      return;
    }
    setPose(yaw, pitch);
    raf = requestAnimationFrame(tick);
  }

  function kick(){ if (live && !raf) raf = requestAnimationFrame(tick); }
  function onMove(e){ ptr = { x: e.clientX, y: e.clientY }; kick(); }
  function onLeave(){ ptr = null; kick(); }

  function listen(){
    if (live || !cube) return;
    live = true;
    window.addEventListener("pointermove", onMove, { passive: true });
    document.documentElement.addEventListener("mouseleave", onLeave);
  }
  function unlisten(){
    live = false;
    window.removeEventListener("pointermove", onMove);
    document.documentElement.removeEventListener("mouseleave", onLeave);
    if (raf) cancelAnimationFrame(raf);
    raf = 0; last = 0; ptr = null;
    if (cube) setPose(0, 0);
  }

  D.head = {
    /* `hostEl` es el contenedor del cubo; `flatCanvas`, el canvas plano que sustituye. */
    mount: function(hostEl, flatCanvas){
      if (!hostEl || cube) return;
      host = hostEl; flat = flatCanvas || null;
      cube = document.createElement("div");
      cube.className = "cube";
      Object.keys(UV).forEach(function(n){ faces[n] = faceCanvas(n); cube.appendChild(faces[n]); });
      host.appendChild(cube);
      D.perf.register("head", {
        pause: unlisten,
        resume: function(){ if (wanted && !reduce) listen(); }
      });
    },
    paint: function(spec){ paint(spec); },
    /* La preferencia del jugador. Apagada: no hay cubo, no hay escucha, vuelve el canvas. */
    setEnabled: function(on){
      wanted = !!on && !reduce;
      if (!cube) return;
      show(wanted);
      if (wanted) { if (D.perf.running()) listen(); } else unlisten();
    },
    supported: !reduce
  };
})();
