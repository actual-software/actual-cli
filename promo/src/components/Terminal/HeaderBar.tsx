import React from "react";
import { COLORS, FONTS } from "../../data/brand";

export const HeaderBar: React.FC = () => (
  <div
    style={{
      fontFamily: FONTS.mono,
      fontSize: 13,
      color: COLORS.textDim,
      padding: "6px 16px",
      borderBottom: `1px solid ${COLORS.borderGreen}22`,
      display: "flex",
      justifyContent: "space-between",
      flexShrink: 0,
    }}
  >
    <span style={{ color: COLORS.textPrimary }}>
      actual{" "}
      <span style={{ color: COLORS.borderGreen }}>v0.1.0</span>
    </span>
    <span style={{ color: COLORS.textDim }}>https://app.actual.ai</span>
  </div>
);
