import React from "react";
import { COLORS, FONTS } from "../../data/brand";

// Matches real TUI header: "actual v0.1.0 ── https://app.actual.ai" left-aligned
export const HeaderBar: React.FC = () => (
  <div
    style={{
      fontFamily: FONTS.mono,
      fontSize: 13,
      padding: "5px 12px",
      borderBottom: `1px solid ${COLORS.borderTeal}33`,
      flexShrink: 0,
      whiteSpace: "pre",
    }}
  >
    <span style={{ color: COLORS.textPrimary }}>actual </span>
    <span style={{ color: COLORS.borderGreen }}>v0.1.0</span>
    <span style={{ color: COLORS.textDim }}> ── </span>
    <span style={{ color: COLORS.borderTeal }}>https://app.actual.ai</span>
  </div>
);
