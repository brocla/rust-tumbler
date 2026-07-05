import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import { PasswordPrompt } from "./PasswordPrompt";
import { usePdfStore } from "../store/usePdfStore";

describe("PasswordPrompt (issue #12)", () => {
  beforeEach(() => {
    usePdfStore.setState({ passwordPrompt: null });
  });

  it("renders nothing when no prompt is pending", () => {
    const { container } = render(<PasswordPrompt />);
    expect(container).toBeEmptyDOMElement();
  });

  it("resolves with the entered password on submit", async () => {
    render(<PasswordPrompt />);
    let resolved: string | null | undefined;
    act(() => {
      void usePdfStore
        .getState()
        .askPassword("secret.pdf", false)
        .then((p) => {
          resolved = p;
        });
    });

    expect(screen.getByText(/secret\.pdf/)).toBeInTheDocument();
    // No "wrong password" hint on the first prompt.
    expect(screen.queryByText(/wrong password/i)).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "open-sesame" },
    });
    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });

    expect(resolved).toBe("open-sesame");
    expect(usePdfStore.getState().passwordPrompt).toBeNull();
  });

  it("resolves with null when cancelled", async () => {
    render(<PasswordPrompt />);
    let resolved: string | null | undefined;
    act(() => {
      void usePdfStore
        .getState()
        .askPassword("secret.pdf", false)
        .then((p) => {
          resolved = p;
        });
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Cancel"));
    });

    expect(resolved).toBeNull();
    expect(usePdfStore.getState().passwordPrompt).toBeNull();
  });

  it("treats an empty submit as a cancel (null)", async () => {
    render(<PasswordPrompt />);
    let resolved: string | null | undefined;
    act(() => {
      void usePdfStore
        .getState()
        .askPassword("secret.pdf", false)
        .then((p) => {
          resolved = p;
        });
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Open"));
    });

    expect(resolved).toBeNull();
  });

  it("shows a retry hint when a prior password was rejected", () => {
    render(<PasswordPrompt />);
    act(() => {
      void usePdfStore.getState().askPassword("secret.pdf", true);
    });
    expect(screen.getByText(/wrong password/i)).toBeInTheDocument();
  });
});
