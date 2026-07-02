import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { UnsavedChangesDialog } from "./UnsavedChangesDialog";
import { usePdfStore } from "../store/usePdfStore";
import type { UnsavedChoice } from "../store/usePdfStore";

describe("UnsavedChangesDialog", () => {
  it("renders nothing while no prompt is pending", () => {
    usePdfStore.setState({ unsavedPrompt: null });
    const { container } = render(<UnsavedChangesDialog />);
    expect(container).toBeEmptyDOMElement();
  });

  it.each<[string, UnsavedChoice]>([
    ["Save", "save"],
    ["Don't Save", "discard"],
    ["Cancel", "cancel"],
  ])("resolves askUnsaved with %j when that button is clicked", async (label, expected) => {
    usePdfStore.setState({ unsavedPrompt: null });
    render(<UnsavedChangesDialog />);

    const pending = usePdfStore.getState().askUnsaved("test.pdf");
    expect(await screen.findByText('Save changes to "test.pdf"?')).toBeInTheDocument();

    fireEvent.click(screen.getByText(label));

    await expect(pending).resolves.toBe(expected);
    expect(usePdfStore.getState().unsavedPrompt).toBeNull();
  });
});
