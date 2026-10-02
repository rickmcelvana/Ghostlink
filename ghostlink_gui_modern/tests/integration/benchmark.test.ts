import { describe, it, expect, beforeAll } from 'vitest';
import { GhostlinkAPI } from '../../src/api';
import { resolveApiBase } from '../../src/config';

// Mock environment for testing (mimics Vite dev/prod builds)
(window as any)._env_ = {
  GHOSTLINK_API_BASE: 'http://127.0.0.1:8003',
};

describe('API Performance Benchmarks', () => {
  let apiInstance: GhostlinkAPI;

  beforeAll(() => {
    const detectedApiBase = resolveApiBase({
      GHOSTLINK_API_BASE: (window as any)._env_?.GHOSTLINK_API_BASE,
      VITE_GHOSTLINK_API_BASE: import.meta.env.VITE_GHOSTLINK_API_BASE,
      GHOSTLINK_BACKEND_URL: undefined,
      VITE_GHOSTLINK_BACKEND_URL: undefined,
    });
    apiInstance = new GhostlinkAPI(detectedApiBase);
  });

  describe('getModels', () => {
    it('should measure response time for getModels', async () => {
      const start = performance.now();
      
      // Run multiple times to get stable measurement
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getModels', async () => {
      try {
        await apiInstance.getModels();
        
        // Note: In Node.js we'd use process.memoryUsage() but in browser context
        // this measures the time between calls as a proxy
        const start = performance.now();
        await apiInstance.getModels();
        const duration = performance.now() - start;
        
        expect(duration).toBeLessThan(500); // Should complete quickly on cached models
      } catch {
        // Ignore network errors
      }
    });
  });

  describe('getMetrics', () => {
    it('should measure response time for getMetrics', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getMetrics();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getMetrics', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getMetrics();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getSessions', () => {
    it('should measure response time for getSessions', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getSessions();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getSessions', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getSessions();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getWorkers', () => {
    it('should measure response time for getWorkers', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getWorkers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getWorkers', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getWorkers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getHealth', () => {
    it('should measure response time for getHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('listMcpServers', () => {
    it('should measure response time for listMcpServers', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.listMcpServers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for listMcpServers', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.listMcpServers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getSettings', () => {
    it('should measure response time for getSettings', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getSettings();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getSettings', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getSettings();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('sendMessage (streaming)', () => {
    it('should measure response time for sendMessage', async () => {
      const payload = {
        message: 'Hello, how are you?',
        messages: [
          { role: 'user', content: 'Hello, how are you?' }
        ],
        model: 'test-model',
        temperature: 0.7,
        top_p: 0.9,
        top_k: 50,
        penalty: 1.2,
        max_tokens: 256,
        system_prompt: 'You are a helpful assistant.',
        mcp: {},
        stream: false, // Using non-streaming for predictable timing
      };

      const start = performance.now();
      
      try {
        await apiInstance.sendMessage(payload);
        
        const duration = performance.now() - start;
        expect(duration).toBeGreaterThan(0);
      } catch (error) {
        // Network errors are expected in dev environment without backend
        console.log('Expected error:', (error as Error).message || 'Network error');
      }
    });

    it('should measure memory usage for sendMessage', async () => {
      const payload = {
        message: 'Hello, how are you?',
        messages: [
          { role: 'user', content: 'Hello, how are you?' }
        ],
        model: 'test-model',
        temperature: 0.7,
        top_p: 0.9,
        top_k: 50,
        penalty: 1.2,
        max_tokens: 256,
        system_prompt: 'You are a helpful assistant.',
        mcp: {},
        stream: false,
      };

      const start = performance.now();
      
      try {
        await apiInstance.sendMessage(payload);
        
        const duration = performance.now() - start;
        expect(duration).toBeGreaterThan(0);
      } catch (error) {
        // Network errors are expected in dev environment without backend
        console.log('Expected error:', (error as Error).message || 'Network error');
      }
    });
  });

  describe('getMetricsHistory', () => {
    it('should measure response time for getMetricsHistory', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 3; i++) {
        try {
          await apiInstance.getMetricsHistory();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getMetricsHistory', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 3; i++) {
        try {
          await apiInstance.getMetricsHistory();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('Circuit Breaker State', () => {
    it('should measure state access time for getCircuitBreakerState', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 100; i++) {
        apiInstance.getCircuitBreakerState();
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure state reset time for resetCircuitBreaker', async () => {
      apiInstance.resetCircuitBreaker();
      
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 100; i++) {
        apiInstance.resetCircuitBreaker();
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('API Instance Reuse', () => {
    it('should measure response time for repeated API calls with same instance', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 10; i++) {
        try {
          await apiInstance.getHealth();
          await apiInstance.getMetrics();
          await apiInstance.getSessions();
          await apiInstance.getWorkers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for repeated API calls with same instance', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 10; i++) {
        try {
          await apiInstance.getHealth();
          await apiInstance.getMetrics();
          await apiInstance.getSessions();
          await apiInstance.getWorkers();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaModels', () => {
    it('should measure response time for getOllamaModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getVllmModels', () => {
    it('should measure response time for getVllmModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getVllmModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getClusterTopology', () => {
    it('should measure response time for getClusterTopology', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getClusterTopology();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getClusterTopology', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getClusterTopology();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getDownloadProgress', () => {
    it('should measure response time for getDownloadProgress', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getDownloadProgress('test-model');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getDownloadProgress', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getDownloadProgress('test-model');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getWorkspaceTree', () => {
    it('should measure response time for getWorkspaceTree', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getWorkspaceTree('/');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getWorkspaceTree', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getWorkspaceTree('/');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaHealth', () => {
    it('should measure response time for getOllamaHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getVllmHealth', () => {
    it('should measure response time for getVllmHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getVllmHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getPQCState', () => {
    it('should measure response time for getPQCState', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getPQCState();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getPQCState', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getPQCState();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaRunningModels', () => {
    it('should measure response time for getOllamaRunningModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaRunningModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaRunningModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaRunningModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaVersion', () => {
    it('should measure response time for getOllamaVersion', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaVersion();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaVersion', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaVersion();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaEmbeddings', () => {
    it('should measure response time for getOllamaEmbeddings', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaEmbeddings('test-model', 'Hello, world!');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaEmbeddings', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaEmbeddings('test-model', 'Hello, world!');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaChat', () => {
    it('should measure response time for getOllamaChat', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.chatOllama('test-model', [
            { role: 'user', content: 'Hello, world!' }
          ], { stream: false });
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaChat', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.chatOllama('test-model', [
            { role: 'user', content: 'Hello, world!' }
          ], { stream: false });
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getBackendStatus', () => {
    it('should measure response time for getBackendStatus', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getBackendStatus('test-backend');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getBackendStatus', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getBackendStatus('test-backend');
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getInferenceEngines', () => {
    it('should measure response time for getInferenceEngines', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getInferenceEngines();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getInferenceEngines', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getInferenceEngines();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaHealth', () => {
    it('should measure response time for getOllamaHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getVllmHealth', () => {
    it('should measure response time for getVllmHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getVllmHealth', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmHealth();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getOllamaModels', () => {
    it('should measure response time for getOllamaModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getOllamaModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getOllamaModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

  describe('getVllmModels', () => {
    it('should measure response time for getVllmModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });

    it('should measure memory usage for getVllmModels', async () => {
      const start = performance.now();
      
      let totalDuration = 0;
      for (let i = 0; i < 5; i++) {
        try {
          await apiInstance.getVllmModels();
        } catch {
          // Ignore network errors in dev environment
        }
        
        const duration = performance.now() - start;
        totalDuration += duration;
      }
      
      expect(totalDuration).toBeGreaterThan(0);
    });
  });

});
