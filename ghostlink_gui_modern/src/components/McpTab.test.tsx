import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { McpTab } from './McpTab';
import { useAppStore } from '../store';

function createMockApi() {
  return {
    listMcpServers: vi.fn().mockResolvedValue({
      servers: [],
    }),
    toggleMcpServer: vi.fn().mockResolvedValue({ success: true, servers: [] }),
    deleteMcpServer: vi.fn().mockResolvedValue({ success: true, servers: [] }),
    createMcpServer: vi.fn().mockResolvedValue({ success: true, servers: [] }),
    updateMcpServer: vi.fn().mockResolvedValue({ success: true, servers: [] }),
  };
}

describe('McpTab', () => {
  beforeEach(() => {
    useAppStore.setState({
      mcpServers: [],
      setMcpServers: (servers) => useAppStore.setState({ mcpServers: servers }),
      addToast: vi.fn(),
    } as any);
  });

  it('renders empty state with CTA button that opens the modal', async () => {
    const api = createMockApi();
    render(<McpTab api={api} />);

    await waitFor(() => {
      expect(screen.getByText('No MCP servers configured')).toBeInTheDocument();
    });

    const addButtons = screen.getAllByRole('button', { name: /Add Server/i });
    expect(addButtons.length).toBeGreaterThan(0);

    fireEvent.click(addButtons[0]);

    await waitFor(() => {
      expect(screen.getByRole('dialog', { name: 'Add MCP Server' })).toBeInTheDocument();
    });
  });

  it('traps focus inside the dialog when Tab key is pressed', async () => {
    const api = createMockApi();
    render(<McpTab api={api} />);

    await waitFor(() => {
      expect(screen.getByText('No MCP servers configured')).toBeInTheDocument();
    });

    const addButton = screen.getAllByRole('button', { name: /Add Server/i })[0];
    fireEvent.click(addButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog', { name: 'Add MCP Server' })).toBeInTheDocument();
    });

    const closeButton = screen.getByRole('button', { name: 'Close' });
    const cancelButton = screen.getByRole('button', { name: 'Cancel editing server' });

    cancelButton.focus();
    expect(document.activeElement).toBe(cancelButton);

    // Tab from last focusable item (Save button) should cycle back to first (Close button)
    const saveButton = screen.getByRole('button', { name: 'Add MCP server' });
    saveButton.focus();
    expect(document.activeElement).toBe(saveButton);

    fireEvent.keyDown(document.activeElement!, { key: 'Tab', shiftKey: false });
    expect(document.activeElement).toBe(closeButton);

    // Shift+Tab from first focusable item (Close button) should cycle back to last (Save button)
    fireEvent.keyDown(document.activeElement!, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(saveButton);
  });

  it('restores focus to trigger button when modal is closed', async () => {
    const api = createMockApi();
    render(<McpTab api={api} />);

    await waitFor(() => {
      expect(screen.getByText('No MCP servers configured')).toBeInTheDocument();
    });

    const addButton = screen.getAllByRole('button', { name: /Add Server/i })[0];
    fireEvent.click(addButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog', { name: 'Add MCP Server' })).toBeInTheDocument();
    });

    const closeButton = screen.getByRole('button', { name: 'Close' });
    fireEvent.click(closeButton);

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
      expect(document.activeElement).toBe(addButton);
    });
  });

  it('disables Refresh button and sets aria-busy while refreshing MCP servers', async () => {
    let resolveRefresh: (val: any) => void;
    const pendingRefresh = new Promise((resolve) => {
      resolveRefresh = resolve;
    });

    const api = createMockApi();
    api.listMcpServers.mockImplementationOnce(() => pendingRefresh);

    render(<McpTab api={api} />);

    const refreshBtn = screen.getByRole('button', { name: 'Refreshing MCP servers...' });
    expect(refreshBtn).toBeDisabled();
    expect(refreshBtn).toHaveAttribute('aria-busy', 'true');
    expect(refreshBtn).toHaveAttribute('title', 'Refreshing MCP servers...');

    resolveRefresh!({ servers: [] });

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'Refresh MCP servers' })).not.toBeDisabled();
    });

    const idleRefreshBtn = screen.getByRole('button', { name: 'Refresh MCP servers' });
    expect(idleRefreshBtn).toHaveAttribute('aria-busy', 'false');
    expect(idleRefreshBtn).toHaveAttribute('title', 'Refresh MCP servers');
  });

  it('sets aria-busy, title tooltip, and spinner on delete button during server deletion', async () => {
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    let resolveDelete: (val: any) => void;
    const pendingDelete = new Promise((resolve) => {
      resolveDelete = resolve;
    });

    const api = createMockApi();
    api.listMcpServers.mockResolvedValue({
      servers: [
        {
          name: 'test-server',
          slot: 'test',
          enabled: true,
          requires_confirmation: false,
          timeout_secs: 30,
          connected: true,
          tool_count: 3,
          transport: { transport: 'stdio', command: 'npx', args: [], env: {} },
        },
      ],
    });
    api.deleteMcpServer.mockImplementationOnce(() => pendingDelete);

    render(<McpTab api={api} />);

    await waitFor(() => {
      expect(screen.getByText('test-server')).toBeInTheDocument();
    });

    const removeBtn = screen.getByRole('button', { name: 'Remove test-server' });
    fireEvent.click(removeBtn);

    await waitFor(() => {
      const deletingBtn = screen.getByRole('button', { name: 'Removing test-server...' });
      expect(deletingBtn).toBeDisabled();
      expect(deletingBtn).toHaveAttribute('aria-busy', 'true');
      expect(deletingBtn).toHaveAttribute('title', 'Removing test-server...');
    });

    resolveDelete!({ success: true, servers: [] });

    await waitFor(() => {
      expect(screen.queryByRole('button', { name: 'Removing test-server...' })).not.toBeInTheDocument();
    });
  });

  it('sets aria-busy and disables modal buttons while saving server', async () => {
    let resolveSave: (val: any) => void;
    const pendingSave = new Promise((resolve) => {
      resolveSave = resolve;
    });

    const api = createMockApi();
    api.createMcpServer.mockImplementationOnce(() => pendingSave);

    render(<McpTab api={api} />);

    await waitFor(() => {
      expect(screen.getByText('No MCP servers configured')).toBeInTheDocument();
    });

    const addButton = screen.getAllByRole('button', { name: /Add Server/i })[0];
    fireEvent.click(addButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog', { name: 'Add MCP Server' })).toBeInTheDocument();
    });

    const nameInput = document.getElementById('mcp-name')!;
    const commandInput = document.getElementById('mcp-command')!;

    fireEvent.change(nameInput, { target: { value: 'new-server' } });
    fireEvent.change(commandInput, { target: { value: 'node' } });

    const saveBtn = screen.getByRole('button', { name: 'Add MCP server' });
    fireEvent.click(saveBtn);

    await waitFor(() => {
      const savingBtn = screen.getByRole('button', { name: 'Saving server...' });
      expect(savingBtn).toBeDisabled();
      expect(savingBtn).toHaveAttribute('aria-busy', 'true');
      expect(savingBtn).toHaveAttribute('title', 'Saving server...');

      const cancelBtn = screen.getByRole('button', { name: 'Cancel editing server' });
      expect(cancelBtn).toBeDisabled();
    });

    resolveSave!({ success: true, servers: [] });

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });
});
