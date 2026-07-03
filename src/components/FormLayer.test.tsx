import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { FormLayer } from "./FormLayer";
import { usePdfStore } from "../store/usePdfStore";

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

  it("commits a single-line text field on Enter", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") return fields; // includes single-line fullName
      return undefined;
    });
    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });
    const textbox = await screen.findByRole("textbox");
    textbox.focus(); // blur() only fires a blur event on the focused element
    fireEvent.change(textbox, { target: { value: "Grace" } });
    fireEvent.keyDown(textbox, { key: "Enter" });
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_form_field_value", {
        docId: "doc-1",
        fieldId: "fullName",
        value: "Grace",
      }),
    );
  });

  it("caps a /MaxLen text field via the maxLength attribute", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields")
        return [
          {
            id: "ssn",
            name: "ssn",
            fieldType: "text",
            value: "",
            exportValue: "",
            rect: { x: 50, y: 50, width: 150, height: 20 },
            page: 1,
            options: [],
            readOnly: false,
            maxLen: 9,
            comb: true,
            label: "",
            buttonAction: "none",
          },
        ];
      return undefined;
    });
    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });
    const input = (await screen.findByRole("textbox")) as HTMLInputElement;
    expect(input.maxLength).toBe(9);
  });

  it("renders nothing when the page has no fields", async () => {
    mockInvoke.mockImplementation(async () => []);
    const { container } = render(
      <FormLayer docId="doc-1" pageNumber={1} zoom={100} />,
    );
    await waitFor(() => expect(container.querySelector(".form-layer")).toBeNull());
  });

  const buttonField = (id: string, action: string) => ({
    id,
    name: id,
    fieldType: "button",
    value: "",
    exportValue: "",
    rect: { x: 50, y: 50, width: 80, height: 20 },
    page: 1,
    options: [],
    readOnly: false,
    label: id,
    buttonAction: action,
  });

  it("invokes reset_form_via_button for a ResetForm button", async () => {
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") return [buttonField("resetBtn", "reset_form")];
      return undefined;
    });
    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });
    fireEvent.click(await screen.findByRole("button"));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("reset_form_via_button", {
        docId: "doc-1",
        fieldId: "resetBtn",
      }),
    );
  });

  it("clears a text field after a reset (formEpoch bump re-fetches cleared value)", async () => {
    // A reset changes the buffer and bumps formEpoch; FormLayer re-fetches the
    // cleared value and re-renders. Text inputs must be controlled — with an
    // uncontrolled defaultValue, React reuses the DOM node and the box keeps the
    // pre-reset text (the .value property doesn't update). This guards that.
    usePdfStore.setState({
      tabs: [{ docId: "doc-1", pagesVersion: 0, formEpoch: 0 } as never],
    });
    let fetched = "typed";
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") {
        return [
          {
            id: "hasDefault",
            name: "hasDefault",
            fieldType: "text",
            value: fetched,
            exportValue: "",
            rect: { x: 50, y: 50, width: 200, height: 20 },
            page: 1,
            options: [],
            readOnly: false,
            label: "",
            buttonAction: "none",
          },
        ];
      }
      return undefined;
    });

    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });
    const input = await screen.findByRole("textbox");
    expect((input as HTMLInputElement).value).toBe("typed");

    // Simulate the reset: the backend cleared the field, so the next fetch
    // returns the empty value; bumping formEpoch triggers the re-fetch.
    fetched = "";
    await act(async () => {
      usePdfStore.getState().bumpFormEpoch("doc-1");
    });

    await waitFor(() =>
      expect((screen.getByRole("textbox") as HTMLInputElement).value).toBe(""),
    );

    usePdfStore.setState({ tabs: [] });
  });

  it("shows a notice (and does not call reset) for an unsupported button", async () => {
    usePdfStore.getState().clearNotice();
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_form_fields") return [buttonField("jsBtn", "unsupported")];
      return undefined;
    });
    await act(async () => {
      render(<FormLayer docId="doc-1" pageNumber={1} zoom={100} />);
    });
    fireEvent.click(await screen.findByRole("button"));
    await waitFor(() =>
      expect(usePdfStore.getState().notice).toBe(
        "This button's action is not supported",
      ),
    );
    expect(mockInvoke).not.toHaveBeenCalledWith(
      "reset_form_via_button",
      expect.anything(),
    );
  });
});
