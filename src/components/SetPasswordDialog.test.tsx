import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SetPasswordDialog } from "./SetPasswordDialog";

function renderDialog(overrides: Partial<Parameters<typeof SetPasswordDialog>[0]> = {}) {
  const onSubmit = vi.fn();
  const onCancel = vi.fn();
  render(
    <SetPasswordDialog
      fileName="test.pdf"
      changing={false}
      onSubmit={onSubmit}
      onCancel={onCancel}
      {...overrides}
    />,
  );
  return { onSubmit, onCancel };
}

describe("SetPasswordDialog (issue #58)", () => {
  it("disables submit until the password is entered and confirmed", () => {
    renderDialog();
    const submit = screen.getByText("Set Password");
    expect(submit).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "pw" },
    });
    expect(submit).toBeDisabled();

    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "pw" },
    });
    expect(submit).toBeEnabled();
  });

  it("shows a mismatch hint and keeps submit disabled", () => {
    const { onSubmit } = renderDialog();
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "pw-one" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "pw-two" },
    });

    expect(screen.getByText(/don't match/)).toBeInTheDocument();
    expect(screen.getByText("Set Password")).toBeDisabled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("submits the matching password", () => {
    const { onSubmit } = renderDialog();
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "secret" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "secret" },
    });
    fireEvent.click(screen.getByText("Set Password"));

    expect(onSubmit).toHaveBeenCalledWith("secret");
  });

  it("cancel calls onCancel without submitting", () => {
    const { onSubmit, onCancel } = renderDialog();
    fireEvent.click(screen.getByText("Cancel"));

    expect(onCancel).toHaveBeenCalled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("uses change wording when the document is already protected", () => {
    renderDialog({ changing: true });
    expect(screen.getByText(/Change the password for "test.pdf"/)).toBeInTheDocument();
    expect(screen.getByText("Change Password")).toBeInTheDocument();
  });
});
