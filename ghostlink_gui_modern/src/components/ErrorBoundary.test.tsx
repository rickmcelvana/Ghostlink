import React, { useState } from 'react';
import { render, screen, fireEvent } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { ErrorBoundary, ConnectionErrorBoundary, OfflineBanner } from './ErrorBoundary';

const ProblemChild = ({ shouldThrow }: { shouldThrow: boolean }) => {
  if (shouldThrow) {
    throw new Error('Test component crashed');
  }
  return <div>Component loaded successfully</div>;
};

const ResettableTestComponent = () => {
  const [shouldThrow, setShouldThrow] = useState(true);

  return (
    <div>
      <button onClick={() => setShouldThrow(false)}>Fix Error</button>
      <ErrorBoundary>
        <ProblemChild shouldThrow={shouldThrow} />
      </ErrorBoundary>
    </div>
  );
};

describe('ErrorBoundary', () => {
  it('renders children when no error occurs', () => {
    render(
      <ErrorBoundary>
        <ProblemChild shouldThrow={false} />
      </ErrorBoundary>
    );
    expect(screen.getByText('Component loaded successfully')).toBeInTheDocument();
  });

  it('renders fallback UI with accessible Try Again button and details summary when an error is caught', () => {
    const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <ErrorBoundary>
        <ProblemChild shouldThrow={true} />
      </ErrorBoundary>
    );

    const alertEl = screen.getByRole('alert');
    expect(alertEl).toBeInTheDocument();
    expect(screen.getByText('Something went wrong')).toBeInTheDocument();
    expect(screen.getByText('Test component crashed')).toBeInTheDocument();

    const summaryEl = screen.getByText('Error Details');
    expect(summaryEl).toHaveAttribute('aria-label', 'Error Details - Toggle error stack trace details');
    expect(summaryEl).toHaveAttribute('title', 'Toggle error stack trace details');

    const retryBtn = screen.getByRole('button', { name: /Try Again/i });
    expect(retryBtn).toBeInTheDocument();
    expect(retryBtn).toHaveAttribute('aria-label', 'Try Again - Retry recovering from application error');
    expect(retryBtn).toHaveAttribute('title', 'Try Again - Retry recovering from application error');

    consoleSpy.mockRestore();
  });

  it('resets error state when Try Again is clicked', () => {
    const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(<ResettableTestComponent />);

    expect(screen.getByText('Something went wrong')).toBeInTheDocument();

    // Fix the underlying state so the child no longer throws
    fireEvent.click(screen.getByText('Fix Error'));

    // Click "Try Again" on the ErrorBoundary to clear the error boundary state
    const retryBtn = screen.getByRole('button', { name: /Try Again/i });
    fireEvent.click(retryBtn);

    expect(screen.getByText('Component loaded successfully')).toBeInTheDocument();

    consoleSpy.mockRestore();
  });
});

describe('ConnectionErrorBoundary', () => {
  it('renders children when isOnline is true', () => {
    render(
      <ConnectionErrorBoundary isOnline={true}>
        <div>Online Content</div>
      </ConnectionErrorBoundary>
    );
    expect(screen.getByText('Online Content')).toBeInTheDocument();
  });

  it('renders offline alert and accessible Reconnect button when isOnline is false', () => {
    render(
      <ConnectionErrorBoundary isOnline={false}>
        <div>Online Content</div>
      </ConnectionErrorBoundary>
    );

    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByText('Connection Lost')).toBeInTheDocument();

    const reconnectBtn = screen.getByRole('button', { name: /Reconnect/i });
    expect(reconnectBtn).toBeInTheDocument();
    expect(reconnectBtn).toHaveAttribute('aria-label', 'Reconnect - Reconnect to server and reload page');
    expect(reconnectBtn).toHaveAttribute('title', 'Reconnect - Reconnect to server and reload page');
  });
});

describe('OfflineBanner', () => {
  it('renders nothing when online', () => {
    const { container } = render(<OfflineBanner isOnline={true} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders status banner with polite aria-live when offline', () => {
    render(<OfflineBanner isOnline={false} />);
    const statusEl = screen.getByRole('status');
    expect(statusEl).toBeInTheDocument();
    expect(statusEl).toHaveAttribute('aria-live', 'polite');
    expect(screen.getByText('Attempting to reconnect...')).toBeInTheDocument();
  });
});
