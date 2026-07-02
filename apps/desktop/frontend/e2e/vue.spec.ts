import { test, expect } from '@playwright/test'

test('home page loads', async ({ page }) => {
  await page.goto('/')
  // Verify the shell renders (sidebar should be present)
  await expect(page.locator('[data-sidebar="root"]')).toBeVisible()
})

test('markdown review page is accessible', async ({ page }) => {
  await page.goto('/markdown-review')
  await expect(page.locator('[data-sidebar="root"]')).toBeVisible()
  await expect(page).toHaveURL('/markdown-review')
})
