import { test, expect } from "../fixtures/weaver";

test.describe("channels mailbox", () => {
  test("shows session channels, typed status, read acknowledgement, and delivery receipts", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "build a durable communication pipe",
      name: "channel-work",
      title: "Channel work",
    });
    await weaver.setStatus(session, "attention", "review the channel boundary");

    const before = await page.request.get(`${weaver.baseUrl}/api/channels`);
    const channels = (await before.json()) as {
      id: string;
      unread_count: number;
      unread_urgent_count: number;
    }[];
    const unread = channels.find((channel) => channel.id === session.id)!;
    expect(unread.unread_count).toBe(1);
    expect(unread.unread_urgent_count).toBe(1);

    await page.goto(`${weaver.baseUrl}/channels/${session.id}`);
    await expect(page.getByTestId("channels-view")).toBeVisible();
    await expect(
      page.locator(`[data-channel-id="${session.id}"]`),
    ).toContainText("Channel work");
    const messages = page.getByTestId("channel-messages");
    await expect(messages).toContainText("build a durable communication pipe");
    await expect(messages).toContainText("review the channel boundary");
    await expect(messages).toContainText("status");

    await expect
      .poll(async () => {
        const response = await page.request.get(
          `${weaver.baseUrl}/api/channels`,
        );
        const rows = (await response.json()) as {
          id: string;
          unread_urgent_count: number;
        }[];
        return rows.find((channel) => channel.id === session.id)
          ?.unread_urgent_count;
      })
      .toBe(0);

    const composer = page.getByPlaceholder(/message this channel/);
    await composer.fill("echo channel-delivery");
    await composer.press("Control+Enter");
    await expect(messages).toContainText("echo channel-delivery");
    await expect(messages).toContainText("delivered");

    await page.getByTestId("new-channel").click();
    await page.getByTestId("channel-create-name").fill("release-room");
    await page
      .getByTestId("channel-create-topic")
      .fill("coordinate the release");
    await page.getByRole("button", { name: "open channel" }).click();
    await expect(page).not.toHaveURL(new RegExp(`/channels/${session.id}$`));
    await expect(
      page.getByRole("heading", { name: "release-room" }),
    ).toBeVisible();
    await expect(page.getByText("coordinate the release")).toBeVisible();
    await expect(messages).toContainText("channel is quiet");

    await page.locator(`[data-channel-id="${session.id}"]`).click();
    await page.getByRole("link", { name: /open session/ }).click();
    await expect(page).toHaveURL(new RegExp(`/s/${session.id}$`));
  });
});
