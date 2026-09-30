import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { TaskView } from './TaskView';
import { useAppStore } from '../store';
import { Task } from '../api';

describe('TaskView', () => {
  let mockApi: any;
  let mockEventSourceInstances: any[] = [];
  const OriginalEventSource = globalThis.EventSource;

  const sampleTask: Task = {
    id: 'task_1',
    project_id: 'proj_1',
    goal: 'Test Task Goal',
    status: 'running',
    budget: { max_steps: 10, max_minutes: 5 },
    created_at: '2026-08-10T10:00:00Z',
    updated_at: '2026-08-10T10:05:00Z',
  };

  beforeEach(() => {
    useAppStore.setState({
      taskEvents: [],
      addToast: vi.fn(),
      addTaskEvent: vi.fn(),
    });

    mockEventSourceInstances = [];
    globalThis.EventSource = vi.fn().mockImplementation((url: string) => {
      const instance = {
        url,
        close: vi.fn(),
        onmessage: null,
      };
      mockEventSourceInstances.push(instance);
      return instance;
    }) as any;

    mockApi = {
      getApiKey: vi.fn().mockReturnValue(''),
      getTaskReview: vi.fn().mockResolvedValue(null),
      spawnTask: vi.fn().mockResolvedValue({}),
      cancelTask: vi.fn().mockResolvedValue({}),
    };
  });

  afterEach(() => {
    globalThis.EventSource = OriginalEventSource;
  });

  it('renders API key required notice when getApiKey returns empty string and does not create EventSource', () => {
    mockApi.getApiKey.mockReturnValue('');
    render(<TaskView task={sampleTask} api={mockApi} />);

    expect(screen.getByText(/API key required to stream live task events/i)).toBeInTheDocument();
    expect(mockEventSourceInstances.length).toBe(0);
  });

  it('connects EventSource with encoded access_token query param when API key is set', () => {
    mockApi.getApiKey.mockReturnValue('key_secret+123');
    render(<TaskView task={sampleTask} api={mockApi} />);

    expect(screen.queryByText(/API key required to stream live task events/i)).not.toBeInTheDocument();
    expect(mockEventSourceInstances.length).toBe(1);
    expect(mockEventSourceInstances[0].url).toBe(
      '/api/tasks/task_1/events?access_token=key_secret%2B123'
    );
  });

  it('renders custom brief input with explicit aria-label and spawn button with aria attributes when task is idle', () => {
    const idleTask: Task = { ...sampleTask, status: 'created' };
    render(<TaskView task={idleTask} api={mockApi} />);

    const briefInput = screen.getByLabelText('Custom task brief');
    expect(briefInput).toBeInTheDocument();

    const spawnBtn = screen.getByRole('button', { name: 'Spawn Implementer Run' });
    expect(spawnBtn).toBeInTheDocument();
    expect(spawnBtn).toHaveAttribute('aria-busy', 'false');
    expect(spawnBtn).toHaveAttribute('title', 'Spawn an implementer agent run for this task');
  });

  it('renders cancel button with proper accessibility attributes when task is running', () => {
    render(<TaskView task={sampleTask} api={mockApi} />);

    const cancelBtn = screen.getByRole('button', { name: 'Cancel Task' });
    expect(cancelBtn).toBeInTheDocument();
    expect(cancelBtn).toHaveAttribute('aria-busy', 'false');
    expect(cancelBtn).toHaveAttribute('title', 'Cancel the active task run');
  });
});
