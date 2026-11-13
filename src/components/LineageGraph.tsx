import { useEffect, useRef } from "react";

import { truncateChars } from "../format";
import { graphPalette } from "../theme";
import type { Graph, GraphNode } from "../types";

interface Particle {
  id: string;
  x: number;
  y: number;
  vx: number;
  vy: number;
  node: GraphNode;
}

function radiusOf(weight: number): number {
  return 9 + Math.min(weight, 3) * 4;
}

/** Small force-directed layout, drawn directly on a canvas. */
export function LineageGraph({ graph }: { graph: Graph }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || graph.nodes.length === 0) return;

    // Colours come from the CSS custom properties (single source of truth with
    // the DOM legend in `App.tsx`, see `src/theme.ts`).
    const palette = graphPalette();

    let particles: Particle[] = [];
    let width = 320;
    let height = 220;
    let frame = 0;

    const measure = () => {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      width = Math.max(canvas.clientWidth, 280);
      height = Math.max(canvas.clientHeight, 190);
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      const ctx = canvas.getContext("2d");
      if (ctx) ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    };

    const seed = () => {
      particles = graph.nodes.map((node, i) => {
        // Deterministic golden-angle spiral keeps the layout stable on refresh.
        const angle = i * 2.399963;
        const ring = 34 + Math.sqrt(i + 1) * 26;
        return {
          id: node.id,
          x: width / 2 + Math.cos(angle) * ring * 0.55,
          y: height / 2 + Math.sin(angle) * ring * 0.42,
          vx: 0,
          vy: 0,
          node,
        };
      });
    };

    const step = (alpha: number) => {
      // Pairwise repulsion.
      for (let i = 0; i < particles.length; i++) {
        for (let j = i + 1; j < particles.length; j++) {
          const a = particles[i];
          const b = particles[j];
          let dx = b.x - a.x;
          let dy = b.y - a.y;
          let d2 = dx * dx + dy * dy;
          if (d2 < 1) {
            dx = (i - j) * 0.7 + 0.3;
            dy = 0.6;
            d2 = dx * dx + dy * dy;
          }
          const d = Math.sqrt(d2);
          const force = 2600 / d2;
          const fx = (dx / d) * force;
          const fy = (dy / d) * force;
          a.vx -= fx;
          a.vy -= fy;
          b.vx += fx;
          b.vy += fy;
        }
      }
      // Springs along edges.
      for (const edge of graph.edges) {
        const a = particles.find((p) => p.id === edge.from);
        const b = particles.find((p) => p.id === edge.to);
        if (!a || !b) continue;
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const d = Math.max(Math.sqrt(dx * dx + dy * dy), 1);
        const desired = edge.kind === "origin" ? 120 : 88;
        const pull = (d - desired) * 0.012;
        const fx = (dx / d) * pull;
        const fy = (dy / d) * pull;
        a.vx += fx;
        a.vy += fy;
        b.vx -= fx;
        b.vy -= fy;
      }
      // Gentle centering, then integrate.
      for (const p of particles) {
        p.vx += (width / 2 - p.x) * 0.006;
        p.vy += (height / 2 - p.y) * 0.006;
        p.vx *= 0.8;
        p.vy *= 0.8;
        p.x += Math.max(-8, Math.min(8, p.vx)) * alpha;
        p.y += Math.max(-8, Math.min(8, p.vy)) * alpha;
        const pad = 34;
        p.x = Math.max(pad, Math.min(width - pad, p.x));
        p.y = Math.max(pad, Math.min(height - pad - 8, p.y));
      }
    };

    const draw = () => {
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, width, height);

      const byId = new Map(particles.map((p) => [p.id, p]));
      for (const edge of graph.edges) {
        const a = byId.get(edge.from);
        const b = byId.get(edge.to);
        if (!a || !b) continue;
        const stroke = palette.edges[edge.kind] ?? palette.edges.evolve;
        ctx.strokeStyle = stroke;
        ctx.lineWidth = edge.kind === "origin" ? 1.8 : 1.2;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();

        // Arrowhead, stopping short of the target circle.
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const len = Math.max(Math.sqrt(dx * dx + dy * dy), 1);
        const ux = dx / len;
        const uy = dy / len;
        const r = radiusOf(b.node.weight) + 3;
        const tx = b.x - ux * r;
        const ty = b.y - uy * r;
        const ah = 6;
        ctx.fillStyle = stroke;
        ctx.beginPath();
        ctx.moveTo(tx, ty);
        ctx.lineTo(tx - ux * ah - uy * ah * 0.5, ty - uy * ah + ux * ah * 0.5);
        ctx.lineTo(tx - ux * ah + uy * ah * 0.5, ty - uy * ah - ux * ah * 0.5);
        ctx.closePath();
        ctx.fill();
      }

      for (const p of particles) {
        const r = radiusOf(p.node.weight);
        const color = palette.nodes[p.node.kind] ?? palette.nodes.version;
        ctx.beginPath();
        ctx.arc(p.x, p.y, r + 5, 0, Math.PI * 2);
        ctx.fillStyle = palette.halo;
        ctx.fill();
        ctx.beginPath();
        ctx.arc(p.x, p.y, r, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.globalAlpha = p.node.kind === "version" ? 0.35 : 0.85;
        ctx.fill();
        ctx.globalAlpha = 1;
        ctx.lineWidth = 1.6;
        ctx.strokeStyle = color;
        ctx.stroke();

        ctx.textAlign = "center";
        ctx.textBaseline = "top";
        ctx.font =
          "11px Inter, -apple-system, 'Segoe UI', 'PingFang SC', 'Microsoft YaHei', sans-serif";
        ctx.fillStyle = palette.label;
        ctx.fillText(truncateChars(p.node.label, 18), p.x, p.y + r + 6);
      }
    };

    const rebuild = () => {
      measure();
      seed();
      for (let i = 0; i < 260; i++) step(1 - i / 300);
      draw();
    };

    rebuild();
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(rebuild);
    });
    observer.observe(canvas);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, [graph]);

  if (graph.nodes.length === 0) {
    return (
      <p className="graph-empty muted small">
        还没有可用于绘图的证据节点：等采集到来源或写入事件后这里会出现血缘图。
      </p>
    );
  }

  return (
    <>
      <canvas
        className="graph-canvas"
        ref={canvasRef}
        aria-hidden="true"
        role="presentation"
      />
      <p className="graph-summary muted small">
        {graph.nodes.length} 个节点、{graph.edges.length} 条关系：
        {graph.nodes
          .slice(0, 6)
          .map((node) => node.label)
          .join("、")}
        {graph.nodes.length > 6 ? " …" : ""}
      </p>
    </>
  );
}
