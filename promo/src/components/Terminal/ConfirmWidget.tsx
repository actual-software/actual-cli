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
  selected,
}) => {
  // Render buttons as plain monospace text, matching the real TUI:
  // [>Accept<] [ Change ] [ Reject ]
  const renderBtn = (label: string, choice: ConfirmChoice) => {
    const isSelected = selected === choice;
    return (
      <span
        key={choice}
        style={{
          fontFamily: FONTS.mono,
          fontSize: 13,
          color: isSelected ? COLORS.borderGreen : COLORS.textDim,
          marginRight: 8,
        }}
      >
        {isSelected ? `[>${label}<]` : `[ ${label} ]`}
      </span>
    );
  };

  return (
    <div
      style={{
        fontFamily: FONTS.mono,
        fontSize: 13,
        color: COLORS.textPrimary,
        marginTop: 8,
        lineHeight: 1.7,
      }}
    >
      <div>Proceed with adr-bot?</div>
      <div>
        {renderBtn("Accept", "accept")}
        {renderBtn("Change", "change")}
        {renderBtn("Reject", "reject")}
      </div>
    </div>
  );
};
