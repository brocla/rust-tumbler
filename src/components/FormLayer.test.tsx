import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { FormLayer } from "./FormLayer";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const mockInvoke = vi.mocked(invoke);

const fields = [
  {
    id: "fullName",
    name: "fullName",
    fieldType: "text",
    value: "",
    exportValue: "",
    rect: { x: 50, y: 72, width: 250, height: 20 },
    page: 1,
    options: [],
    readOnly: false,
  },
  {
    id: "subscribe",
    name: "subscribe",
    fieldType: "checkbox",
    value: "Off",
    exportValue: "Yes",
    rect: { x: 50, y: 217, width: 15, height: 15 },
    page: 1,
    options: [],
    readOnly: false,
  },
  {
    id: "country",
    name: "country",
    fieldType: "dropdown",
    value: "USA",
    exportValue: "",
    rect: { x: 50, y: 292, width: 150, height: 20 },
    page: 1,
    options: ["USA", "Canada", "Mexico"],
    readOnly: false,
  },
];

describe("FormLayer", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("renders a control per field and commits a text edit on blur", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") return fields;
      return undefined;
    });

    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });

    const textbox = await screen.findByRole("textbox");
    expect(screen.getByRole("checkbox")).toBeTruthy();
    expect(screen.getByRole("combobox")).toBeTruthy();

    fireEvent.change(textbox, { target: { value: "Ada" } });
    fireEvent.blur(textbox);

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_form_field_value", {
        docId: "doc-1",
        fieldId: "fullName",
        value: "Ada",
      }),
    );
  });

  it("commits Off when a checkbox is toggled off, exportValue when on", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") return fields;
      return undefined;
    });

    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });

    const checkbox = await screen.findByRole("checkbox");
    fireEvent.click(checkbox); // Off -> on
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_form_field_value", {
        docId: "doc-1",
        fieldId: "subscribe",
        value: "Yes",
      }),
    );
  });

  it("renders nothing when the page has no fields", async () => {
    mockInvoke.mockImplementation(async () => []);
    const { container } = render(
      <FormLayer docId="doc-1" pageNumber={1} zoom={100} />,
    );
    await waitFor(() => expect(container.querySelector(".form-layer")).toBeNull());
  });
});
