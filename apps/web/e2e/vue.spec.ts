import { test, expect } from '@playwright/test'

test('home page loads', async ({ page }) => {
  await page.goto('/')
  // Verify the shell renders (sidebar should be present)
  await expect(page.locator('[data-slot="sidebar-wrapper"]')).toBeVisible()
})

test('markdown review page is accessible', async ({ page }) => {
  await page.goto('/markdown-review')
  await expect(page.locator('[data-slot="sidebar-wrapper"]')).toBeVisible()
  await expect(page).toHaveURL('/markdown-review')
})

test('sidebar toggle collapses to an icon rail and back', async ({ page }) => {
  await page.goto('/')

  // Expanded: the sidebar nav label is visible (exact match — the home page
  // body also mentions 审批 in longer strings).
  const approvalLabel = page.getByText('审批', { exact: true })
  await expect(approvalLabel).toBeVisible()

  // Expanded rail advertises drag-resize; the collapsed rail is click-only
  // and must not keep promising a drag it refuses.
  const rail = page.locator('[data-slot="sidebar-rail"]')
  await expect(rail).toHaveCSS('cursor', 'w-resize')

  // The in-sidebar footer toggle collapses to the icon rail.
  await page.locator('[data-slot="sidebar-toggle"]').click()
  await expect(approvalLabel).toBeHidden()
  await expect(rail).toHaveCSS('cursor', 'pointer')
  const gap = page.locator('[data-slot="sidebar-gap"]')
  await expect(gap).toHaveCSS('width', '48px')

  // Toggling again restores the expanded sidebar with labels.
  await page.locator('[data-slot="sidebar-toggle"]').click()
  await expect(approvalLabel).toBeVisible()
  await expect(gap).toHaveCSS('width', '256px')
})
