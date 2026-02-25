import React from "react";

interface VignetteProps {
  intensity?: number; // 0–1, drives opacity of the darkening
}

export const Vignette: React.FC<VignetteProps> = ({ intensity = 0.55 }) => (
  <div
    style={{
      position: "absolute",
      inset: 0,
      pointerEvents: "none",
      background: `radial-gradient(ellipse at center, transparent 45%, rgba(0,0,0,${intensity}) 100%)`,
    }}
  />
);
