import React from "react";
import { COLORS, FONTS } from "../../data/brand";

export type ConfirmChoice = "accept" | "change" | "reject";

interface FilePreview {
  name: string;
  isNew: boolean;
  ruleCount: number;
  previewLines: string[];
}

interface ConfirmWidgetProps {
  file: FilePreview;
  selected: ConfirmChoice;
}

export const ConfirmWidget: React.FC<ConfirmWidgetProps> = ({
  file,
  selected,
}) => {
  const btn = (label: string, choice: ConfirmChoice) => {
    const isSelected = selected === choice;
    return (
      <span
        style={{
          fontFamily: FONTS.mono,
          fontSize: 13,
          color: isSelected ? COLORS.background : COLORS.textDim,
          background: isSelected ? COLORS.borderGreen : "transparent",
          padding: "1px 8px",
          borderRadius: 3,
          border: `1px solid ${isSelected ? COLORS.borderGreen : COLORS.textDim + "66"}`,
          margin: "0 4px",
          filter: isSelected
            ? `drop-shadow(0 0 6px ${COLORS.borderGreen})`
            : "none",
        }}
      >
        {isSelected ? `>${label}<` : ` ${label} `}
      </span>
    );
  };

  return (
    <div
      style={{
        fontFamily: FONTS.mono,
        fontSize: 13,
        color: COLORS.textPrimary,
        border: `1px solid ${COLORS.borderGreen}44`,
        borderRadius: 6,
        padding: "10px 14px",
        margin: "8px 0",
        lineHeight: 1.7,
      }}
    >
      {/* File header */}
      <div>
        <span style={{ color: COLORS.borderGreen }}>{file.name}</span>
        {file.isNew && (
          <span style={{ color: COLORS.textDim, marginLeft: 8 }}>
            (new file)
          </span>
        )}
        <span style={{ color: COLORS.borderGreen, marginLeft: 8 }}>
          +{file.ruleCount} rule{file.ruleCount !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Preview lines */}
      {file.previewLines.map((line, i) => (
        <div key={i} style={{ color: COLORS.textDim, paddingLeft: 4 }}>
          <span style={{ color: COLORS.borderTeal }}>  </span>
          {line}
        </div>
      ))}

      {/* Spacer */}
      <div style={{ marginTop: 8 }} />

      {/* Buttons */}
      <div style={{ display: "flex", alignItems: "center" }}>
        {btn("Accept", "accept")}
        {btn("Change", "change")}
        {btn("Reject", "reject")}
      </div>
    </div>
  );
};
