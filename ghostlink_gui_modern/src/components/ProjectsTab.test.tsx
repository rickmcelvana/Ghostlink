import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { ProjectsTab } from './ProjectsTab';
import { useAppStore } from '../store';

vi.mock('@monaco-editor/react', () => {
  const DiffEditor = ({ original, modified }: any) => {
    return (
      <div data-testid="mock-diff-editor">
        <div>{original}</div>
        <div>{modified}</div>
      </div>
    );
  };
  return {
    __esModule: true,
    default: () => <div />,
    DiffEditor,
  };
});

describe('ProjectsTab', () => {
  let mockApi: any;

  beforeEach(() => {
    useAppStore.setState({
      projects: [],
      activeProject: null,
      tasks: [],
      activeTask: null,
      reviewPacket: null,
      taskEvents: [],
      unreadNeedsReviewCount: 0,
    });

    mockApi = {
      listProjects: vi.fn().mockResolvedValue([
        {
          id: 'proj_1',
          name: 'Core System',
          kind: 'code',
          root_path: '/workspace/core',
          allowed_tools: ['file_operations'],
          created_at: '2026-08-10T10:00:00Z',
        },
      ]),
      listProjectTasks: vi.fn().mockResolvedValue([
        {
          id: 'task_1',
          project_id: 'proj_1',
          goal: 'Implement bugfix',
          status: 'needs_review',
          budget: { max_steps: 24, max_minutes: 15 },
          created_at: '2026-08-10T11:00:00Z',
          updated_at: '2026-08-10T11:05:00Z',
        },
      ]),
      createProject: vi.fn(),
      createTask: vi.fn(),
      getTaskReview: vi.fn().mockResolvedValue(null),
    };
  });

  it('renders project list and fetches project tasks on mount', async () => {
    render(<ProjectsTab api={mockApi} />);

    await waitFor(() => {
      expect(mockApi.listProjects).toHaveBeenCalled();
      expect(screen.getByText('Core System')).toBeInTheDocument();
    });

    await waitFor(() => {
      expect(mockApi.listProjectTasks).toHaveBeenCalledWith('proj_1');
      expect(screen.getByText('Implement bugfix')).toBeInTheDocument();
    });
  });

  it('shows badge when task enters needs_review status', async () => {
    render(<ProjectsTab api={mockApi} />);

    await waitFor(() => {
      expect(screen.getByText('needs_review')).toBeInTheDocument();
      expect(useAppStore.getState().unreadNeedsReviewCount).toBe(1);
    });
  });
});
